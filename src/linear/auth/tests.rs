use super::*;
use crate::test_http::MockServer;
use serde_json::json;

#[test]
fn an_oauth_entry_round_trips_through_its_tags() {
    let tags = tags_for(
        Method::OAuth,
        "kb@ks.dev",
        "Kilospark",
        Some(("LIN_ID", "LIN_SECRET")),
    );
    let t = token_ref_from("LINEAR_KS", &tags).unwrap();
    assert_eq!(t.method, Method::OAuth);
    assert_eq!(t.account, "kb@ks.dev");
    assert_eq!(t.org, "Kilospark");
    assert_eq!(t.client_id_key.as_deref(), Some("LIN_ID"));
    assert_eq!(t.client_secret_key.as_deref(), Some("LIN_SECRET"));
}

#[test]
fn an_api_key_entry_has_no_client() {
    let t = token_ref_from("K", &tags_for(Method::ApiKey, "a@b.c", "O", None)).unwrap();
    assert_eq!(t.method, Method::ApiKey);
    assert!(t.client_id_key.is_none());
    assert!(token_ref_from("LINEAR_DEFAULT_TOKEN", &["linear".into()]).is_none());
}

#[test]
fn api_keys_go_bare_and_oauth_tokens_as_bearer() {
    assert_eq!(header_for("lin_api_123"), "lin_api_123");
    assert_eq!(header_for(" lin_oauth_abc\n"), "Bearer lin_oauth_abc");
}

#[test]
fn the_consent_url_carries_scopes_redirect_and_state() {
    let url = authorize_url(
        "cid",
        &redirect_uri(53695),
        "st",
        &crate::oauth_loopback::Pkce::from_verifier("v".into()),
    );
    assert!(url.starts_with("https://linear.app/oauth/authorize?"));
    let q = crate::oauth_loopback::query_params(&url);
    assert_eq!(q["scope"], "read,write");
    assert_eq!(q["redirect_uri"], "http://localhost:53695/callback");
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["state"], "st");
}

fn tref(k: &str) -> TokenRef {
    token_ref_from(k, &tags_for(Method::ApiKey, "", "", None)).unwrap()
}

#[test]
fn picking_a_token_never_guesses_between_several() {
    let two = vec![tref("A"), tref("B")];
    assert!(pick_token(two.clone(), None, None).is_err());
    assert_eq!(
        pick_token(two.clone(), Some("A".into()), None).unwrap().key,
        "A"
    );
    assert_eq!(pick_token(two, None, Some("B")).unwrap().key, "B");
    let none = pick_token(vec![], None, None).unwrap_err().to_string();
    assert!(none.contains("linear setup"), "{none}");
}

#[test]
fn a_rotated_refresh_token_replaces_the_old_one() {
    let prev = ExpiringToken {
        access_token: "a0".into(),
        refresh_token: Some("r0".into()),
        expires_at: Some(5),
    };
    let next = refreshed(
        &json!({"access_token": "a1", "refresh_token": "r1", "expires_in": 86399}),
        &prev,
        100,
    )
    .unwrap();
    assert_eq!(next.refresh_token.as_deref(), Some("r1"));
    assert_eq!(next.expires_at, Some(86499));
}

#[tokio::test]
async fn token_requests_are_forms_and_errors_are_explained() {
    let server = MockServer::start(|req| {
        if req.form().get("code").map(String::as_str) == Some("good") {
            (
                200,
                json!({"access_token": "t", "refresh_token": "r", "expires_in": 10}).to_string(),
            )
        } else {
            (
                400,
                json!({"error": "invalid_grant", "error_description": "code expired"}).to_string(),
            )
        }
    });
    let http = MockServer::client();
    let ok = token_request(
        &http,
        &server.base,
        &[("code", "good"), ("grant_type", "authorization_code")],
    )
    .await
    .unwrap();
    assert_eq!(ok["access_token"], "t");
    let err = token_request(&http, &server.base, &[("code", "bad")])
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("invalid_grant") && err.contains("code expired"),
        "{err}"
    );
    assert!(
        server.requests()[0].headers["content-type"]
            .starts_with("application/x-www-form-urlencoded")
    );
}

mod refresh_flow {
    use super::super::*;
    use crate::oauth_loopback::{ExpiringToken, now_secs};
    use crate::test_http::MockServer;
    use serde_json::json;

    const KEY: &str = "LIN_REFRESH_TEST";

    fn seed(refresh: &str, expires_in_secs: i64) -> TokenRef {
        crate::broker::kv_set("LIN_CID", "cid", None).unwrap();
        crate::broker::kv_set("LIN_SEC", "secret", None).unwrap();
        let blob = ExpiringToken {
            access_token: "old-access".into(),
            refresh_token: Some(refresh.into()),
            expires_at: Some((now_secs() as i64 + expires_in_secs) as u64),
        };
        let tags = tags_for(
            Method::OAuth,
            "k@x.dev",
            "Acme",
            Some(("LIN_CID", "LIN_SEC")),
        );
        crate::broker::kv_set(KEY, &blob.to_value(), Some(&tags)).unwrap();
        token_ref_from(KEY, &tags).unwrap()
    }

    fn stored() -> ExpiringToken {
        ExpiringToken::parse(&crate::broker::kv_get(KEY).unwrap().unwrap().value)
    }

    fn token_endpoint(access: &'static str, refresh: &'static str) -> MockServer {
        MockServer::start(move |_| {
            (
                200,
                json!({"access_token": access, "refresh_token": refresh, "expires_in": 86399})
                    .to_string(),
            )
        })
    }

    #[tokio::test]
    async fn an_expired_token_is_refreshed_written_back_and_used() {
        let _home = crate::ScratchHome::new();
        let token = seed("refresh-1", -10);
        let server = token_endpoint("new-access", "refresh-2");
        let url = format!("{}/oauth/token", server.base);
        let header = authorization_for_at(&token, &MockServer::client(), &url)
            .await
            .unwrap();
        assert_eq!(header, "Bearer new-access");
        let form = server.requests()[0].form();
        assert_eq!(form["grant_type"], "refresh_token");
        assert_eq!(form["refresh_token"], "refresh-1");
        assert_eq!(form["client_secret"], "secret");
        let now = stored();
        assert_eq!(now.refresh_token.as_deref(), Some("refresh-2"));
        let entry = crate::broker::kv_get(KEY).unwrap().unwrap();
        assert!(
            entry.tags.contains(&"client-id:LIN_CID".to_string()),
            "tags kept"
        );
        assert!(
            crate::broker::kv_history(KEY).unwrap().is_empty(),
            "the spent refresh token is not kept in history"
        );
    }

    #[tokio::test]
    async fn a_fresh_token_makes_no_request() {
        let _home = crate::ScratchHome::new();
        let token = seed("refresh-1", 3600 * 6);
        let server = token_endpoint("x", "y");
        let header = authorization_for_at(&token, &MockServer::client(), &server.base)
            .await
            .unwrap();
        assert_eq!(header, "Bearer old-access");
        assert!(server.requests().is_empty());
    }

    #[tokio::test]
    async fn concurrent_callers_spend_the_refresh_token_once() {
        let _home = crate::ScratchHome::new();
        let token = seed("refresh-1", -10);
        let server = MockServer::start(|_| {
            // Slow enough that the second caller is waiting on the lock.
            std::thread::sleep(std::time::Duration::from_millis(400));
            (
                200,
                json!({"access_token": "new-access", "refresh_token": "refresh-2",
                       "expires_in": 86399})
                .to_string(),
            )
        });
        let http = MockServer::client();
        let (a, b) = tokio::join!(
            authorization_for_at(&token, &http, &server.base),
            authorization_for_at(&token, &http, &server.base),
        );
        assert_eq!(a.unwrap(), "Bearer new-access");
        assert_eq!(b.unwrap(), "Bearer new-access");
        assert_eq!(server.requests().len(), 1, "one refresh, not two");
    }

    #[tokio::test]
    async fn a_failed_refresh_uses_a_token_another_machine_already_rotated() {
        let _home = crate::ScratchHome::new();
        let token = seed("refresh-1", -10);
        let server = MockServer::start(|_| {
            // Meanwhile sync delivers the other machine's fresh blob.
            let fresh = ExpiringToken {
                access_token: "synced-access".into(),
                refresh_token: Some("refresh-9".into()),
                expires_at: Some(now_secs() + 86_000),
            };
            crate::broker::kv_set(KEY, &fresh.to_value(), None).unwrap();
            (400, json!({"error": "invalid_grant"}).to_string())
        });
        let header = authorization_for_at(&token, &MockServer::client(), &server.base)
            .await
            .unwrap();
        assert_eq!(header, "Bearer synced-access");
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test]
    async fn a_failed_refresh_with_nothing_newer_says_how_to_recover() {
        let _home = crate::ScratchHome::new();
        let token = seed("refresh-1", -10);
        let server = MockServer::start(|_| (400, json!({"error": "invalid_grant"}).to_string()));
        let err = authorization_for_at(&token, &MockServer::client(), &server.base)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("invalid_grant") && err.contains("linear login"),
            "{err}"
        );
        assert_eq!(
            stored().refresh_token.as_deref(),
            Some("refresh-1"),
            "kv untouched"
        );
    }
}

#[test]
fn the_linear_authorize_url_carries_a_pkce_challenge() {
    let p = crate::oauth_loopback::Pkce::from_verifier("verifier".into());
    let q = crate::oauth_loopback::query_params(&authorize_url("cid", &redirect_uri(1), "st", &p));
    assert_eq!(q["code_challenge"], p.challenge);
    assert_eq!(q["code_challenge_method"], "S256");
}
