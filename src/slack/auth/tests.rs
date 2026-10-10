use super::*;
use crate::test_http::MockServer;
use serde_json::json;

fn who() -> Identity {
    Identity {
        user: "kb".into(),
        user_id: "U1".into(),
        team: "Kilospark".into(),
        team_id: "T1".into(),
        url: "https://kilospark.slack.com/".into(),
        bot_id: None,
    }
}

#[test]
fn a_token_entry_round_trips_through_its_tags() {
    let tags = tags_for(Kind::User, &who(), Some(("SLACK_ID", "SLACK_SECRET")));
    let t = token_ref_from("SLACK_KS", &tags).expect("is a slack token");
    assert_eq!(t.key, "SLACK_KS");
    assert_eq!(t.kind, Kind::User);
    assert_eq!(t.team, "Kilospark");
    assert_eq!(t.team_id, "T1");
    assert_eq!(t.account, "kb");
    assert_eq!(t.url, "https://kilospark.slack.com/");
    assert_eq!(t.client_id_key.as_deref(), Some("SLACK_ID"));
    assert_eq!(t.client_secret_key.as_deref(), Some("SLACK_SECRET"));
}

#[test]
fn an_adopted_token_has_no_client_to_refresh_with() {
    let t = token_ref_from("B", &tags_for(Kind::Bot, &who(), None)).unwrap();
    assert_eq!(t.kind, Kind::Bot);
    assert!(t.client_id_key.is_none());
}

#[test]
fn other_kv_entries_tagged_slack_are_not_tokens() {
    // The default-pointer entry is tagged slack too; it must not list as a token.
    assert!(token_ref_from("SLACK_DEFAULT_TOKEN", &["slack".into()]).is_none());
    assert!(token_ref_from("X", &[]).is_none());
}

#[test]
fn token_prefixes_say_which_kind() {
    assert_eq!(Kind::from_token("xoxb-1-2"), Some(Kind::Bot));
    assert_eq!(Kind::from_token("xoxp-1-2"), Some(Kind::User));
    assert_eq!(Kind::from_token("xoxe.xoxp-1-2"), Some(Kind::User));
    assert_eq!(Kind::from_token("lin_api_x"), None);
}

fn tref(key: &str) -> TokenRef {
    token_ref_from(key, &tags_for(Kind::User, &who(), None)).unwrap()
}

#[test]
fn picking_a_token_never_guesses_between_several() {
    let two = vec![tref("A"), tref("B")];
    let err = pick_token(two.clone(), None, None).unwrap_err().to_string();
    assert!(err.contains("A, B"), "{err}");
    assert_eq!(
        pick_token(two.clone(), Some("B".into()), None).unwrap().key,
        "B"
    );
    assert_eq!(
        pick_token(two.clone(), Some("B".into()), Some("A"))
            .unwrap()
            .key,
        "A"
    );
    assert!(pick_token(two, None, Some("C")).is_err());
    assert_eq!(
        pick_token(vec![tref("only")], None, None).unwrap().key,
        "only"
    );
    let none = pick_token(vec![], None, None).unwrap_err().to_string();
    assert!(none.contains("slack setup"), "{none}");
}

#[test]
fn a_user_login_asks_for_user_scopes_only() {
    let url = authorize_url(
        "123.456",
        &redirect_uri(53694),
        "st",
        Kind::User,
        Some("T9"),
        None,
    );
    let q = crate::oauth_loopback::query_params(&url);
    assert!(q["user_scope"].contains("search:read"));
    assert!(
        !q.contains_key("scope"),
        "a user login must not also mint a bot token"
    );
    assert_eq!(q["redirect_uri"], "http://localhost:53694/callback");
    assert_eq!(q["state"], "st");
    assert_eq!(q["team"], "T9");
    assert_eq!(q["client_id"], "123.456");
}

#[test]
fn a_bot_login_asks_for_bot_scopes_without_search() {
    let url = authorize_url("id", &redirect_uri(1), "s", Kind::Bot, None, None);
    let q = crate::oauth_loopback::query_params(&url);
    assert!(q["scope"].contains("chat:write"));
    // Slack offers no search scope to bots; asking would fail the install.
    assert!(!q["scope"].contains("search:read"));
    assert!(!q.contains_key("user_scope"));
    assert!(!q.contains_key("team"));
}

#[test]
fn the_exchange_yields_the_token_that_was_asked_for() {
    let res = json!({
        "ok": true,
        "access_token": "xoxb-bot",
        "authed_user": {"id": "U1", "access_token": "xoxp-user"}
    });
    assert_eq!(
        token_from_exchange(&res, Kind::User, 0)
            .unwrap()
            .access_token,
        "xoxp-user"
    );
    assert_eq!(
        token_from_exchange(&res, Kind::Bot, 0)
            .unwrap()
            .access_token,
        "xoxb-bot"
    );
    let bot_only = json!({"ok": true, "access_token": "xoxb-bot", "authed_user": {"id": "U1"}});
    let err = token_from_exchange(&bot_only, Kind::User, 0)
        .unwrap_err()
        .to_string();
    assert!(err.contains("User Token"), "{err}");
}

#[test]
fn a_rotating_token_is_stored_with_its_refresh_token_and_expiry() {
    let res = json!({"ok": true, "authed_user": {
        "access_token": "xoxe.xoxp-1", "refresh_token": "xoxe-1-r", "expires_in": 43200
    }});
    let t = token_from_exchange(&res, Kind::User, 100).unwrap();
    assert_eq!(t.refresh_token.as_deref(), Some("xoxe-1-r"));
    assert_eq!(t.expires_at, Some(43300));
}

#[test]
fn a_refresh_keeps_the_old_refresh_token_when_none_comes_back() {
    let prev = ExpiringToken {
        access_token: "old".into(),
        refresh_token: Some("r0".into()),
        expires_at: Some(1),
    };
    let next = refreshed(
        &json!({"ok": true, "access_token": "new", "expires_in": 60}),
        &prev,
        10,
    )
    .unwrap();
    assert_eq!(next.access_token, "new");
    assert_eq!(next.refresh_token.as_deref(), Some("r0"));
    let rotated = refreshed(
        &json!({"ok": true, "access_token": "n2", "refresh_token": "r1", "expires_in": 60}),
        &prev,
        10,
    )
    .unwrap();
    assert_eq!(rotated.refresh_token.as_deref(), Some("r1"));
}

#[tokio::test]
async fn the_exchange_posts_a_form_and_surfaces_slack_errors() {
    let server = MockServer::sequence(vec![
        json!({"ok": true, "authed_user": {"access_token": "xoxp-1"}}),
        json!({"ok": false, "error": "invalid_code"}),
    ]);
    let http = MockServer::client();
    let form = [
        ("client_id", "id"),
        ("code", "c 1"),
        ("redirect_uri", "http://localhost:1/callback"),
    ];
    exchange(&http, &server.base, &form).await.unwrap();
    let err = exchange(&http, &server.base, &form)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("invalid_code"), "{err}");
    let req = &server.requests()[0];
    assert_eq!(req.path(), "/oauth.v2.access");
    assert!(req.headers["content-type"].starts_with("application/x-www-form-urlencoded"));
    assert_eq!(req.form()["code"], "c 1");
    assert_eq!(req.form()["redirect_uri"], "http://localhost:1/callback");
}

#[test]
fn identity_reads_auth_test() {
    let id = Identity::from_auth_test(&json!({
        "ok": true, "user": "kb", "user_id": "U1", "team": "KS", "team_id": "T1",
        "url": "https://ks.slack.com/", "bot_id": "B1"
    }));
    assert_eq!(id.user, "kb");
    assert_eq!(id.bot_id.as_deref(), Some("B1"));
    assert_eq!(
        Identity::from_auth_test(&json!({"bot_id": ""})).bot_id,
        None
    );
}

#[test]
fn the_manifest_registers_the_redirect_and_disables_rotation() {
    let m = manifest("Sidekar", 53694);
    assert_eq!(
        m["oauth_config"]["redirect_urls"][0],
        "http://localhost:53694/callback"
    );
    assert_eq!(m["settings"]["token_rotation_enabled"], false);
    let user: Vec<&str> = m["oauth_config"]["scopes"]["user"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s.as_str())
        .collect();
    assert_eq!(user, USER_SCOPES);
}

mod refresh_flow {
    use super::super::*;
    use crate::oauth_loopback::{ExpiringToken, now_secs};
    use crate::test_http::MockServer;
    use serde_json::json;

    const KEY: &str = "SLACK_REFRESH_TEST";

    fn seed() -> TokenRef {
        crate::broker::kv_set("SL_CID", "cid", None).unwrap();
        crate::broker::kv_set("SL_SEC", "secret", None).unwrap();
        let blob = ExpiringToken {
            access_token: "xoxe.xoxp-old".into(),
            refresh_token: Some("xoxe-1".into()),
            expires_at: Some(now_secs() - 10),
        };
        let who = Identity::from_auth_test(&json!({"user_id": "U1", "user": "k",
            "team": "Acme", "team_id": "T1", "url": "https://acme.slack.com/"}));
        let tags = tags_for(Kind::User, &who, Some(("SL_CID", "SL_SEC")));
        crate::broker::kv_set(KEY, &blob.to_value(), Some(&tags)).unwrap();
        token_ref_from(KEY, &tags).unwrap()
    }

    #[tokio::test]
    async fn a_rotating_token_refreshes_once_under_the_lock() {
        let _home = crate::ScratchHome::new();
        let token = seed();
        let server = MockServer::start(|_| {
            std::thread::sleep(std::time::Duration::from_millis(300));
            (
                200,
                json!({"ok": true, "access_token": "xoxe.xoxp-new", "refresh_token": "xoxe-2",
                       "expires_in": 43200})
                .to_string(),
            )
        });
        let http = MockServer::client();
        let (a, b) = tokio::join!(
            access_token_for_at(&token, &http, &server.base),
            access_token_for_at(&token, &http, &server.base),
        );
        assert_eq!(a.unwrap(), "xoxe.xoxp-new");
        assert_eq!(b.unwrap(), "xoxe.xoxp-new");
        let reqs = server.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].path(), "/oauth.v2.access");
        assert_eq!(reqs[0].form()["refresh_token"], "xoxe-1");
        let entry = crate::broker::kv_get(KEY).unwrap().unwrap();
        assert_eq!(
            ExpiringToken::parse(&entry.value).refresh_token.as_deref(),
            Some("xoxe-2")
        );
        assert!(entry.tags.contains(&"team-id:T1".to_string()));
        assert!(crate::broker::kv_history(KEY).unwrap().is_empty());
    }

    #[test]
    fn team_checks_accept_the_id_or_the_name() {
        let who = Identity::from_auth_test(&json!({"team": "Acme", "team_id": "T1"}));
        assert!(team_matches("T1", &who));
        assert!(team_matches("acme", &who));
        assert!(!team_matches("T2", &who));
    }

    #[test]
    fn pkce_goes_into_the_authorize_url_only_when_asked() {
        let p = crate::oauth_loopback::Pkce::from_verifier("v".into());
        let with = authorize_url("id", &redirect_uri(1), "s", Kind::User, None, Some(&p));
        let q = crate::oauth_loopback::query_params(&with);
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], p.challenge);
        let without = authorize_url("id", &redirect_uri(1), "s", Kind::User, None, None);
        assert!(!without.contains("code_challenge"));
    }
}
