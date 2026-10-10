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
    let url = authorize_url("cid", &redirect_uri(53695), "st");
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
