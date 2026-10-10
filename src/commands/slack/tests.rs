use super::*;

fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_switch_does_not_eat_the_next_flag() {
    // Without switch awareness, --broadcast swallowed --text and "hi" became a
    // second positional.
    let a = v(&["general", "--broadcast", "--text", "hi"]);
    assert_eq!(positional_with_switches(&a, SWITCHES), vec!["general"]);
    assert_eq!(flag(&a, "--text").as_deref(), Some("hi"));
}

#[test]
fn every_usage_line_names_a_real_subcommand() {
    for verb in [
        "setup",
        "login",
        "add",
        "accounts",
        "use",
        "status",
        "doctor",
        "logout",
        "channels",
        "read",
        "search",
        "users",
        "user",
        "send",
        "dm",
        "draft",
        "bookmarks",
    ] {
        let tokens: Vec<&str> = USAGE
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
            .collect();
        assert!(tokens.contains(&verb), "usage is missing {verb}");
    }
}

#[test]
fn the_port_defaults_and_rejects_garbage() {
    assert_eq!(port_flag(&v(&[])).unwrap(), auth::DEFAULT_PORT);
    assert_eq!(port_flag(&v(&["--port", "8000"])).unwrap(), 8000);
    assert!(port_flag(&v(&["--port", "x"])).is_err());
}

#[test]
fn the_walkthrough_fills_in_every_key_and_the_redirect() {
    let w = setup_walkthrough("Sidekar", 53694, "SLACK_T", "SLACK_ID", "SLACK_SECRET");
    assert!(w.contains("http://localhost:53694/callback"));
    assert!(w.contains("--token SLACK_T --client-id SLACK_ID --client-secret SLACK_SECRET"));
    assert!(w.contains("\"token_rotation_enabled\": false"));
    assert!(w.contains("search:read"));
}

#[test]
fn uploads_read_every_file_first_and_refuse_missing_ones() {
    let dir = std::env::temp_dir().join(format!("sidekar-slack-up-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("a.txt");
    std::fs::write(&a, "hi").unwrap();
    let ok = uploads_from(&[a.display().to_string()], Some("T".into())).unwrap();
    assert_eq!(
        (ok[0].name.as_str(), ok[0].title.as_deref()),
        ("a.txt", Some("T"))
    );
    let missing = dir.join("nope.txt").display().to_string();
    assert!(uploads_from(&[a.display().to_string(), missing], None).is_err());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn attach_values_are_not_positionals() {
    let pos = positional_with_switches(&v(&["#eng", "--attach", "a.pdf", "--print"]), SWITCHES);
    assert_eq!(pos, ["#eng"]);
}

#[test]
fn the_manifest_asks_for_file_scopes() {
    for scopes in [auth::USER_SCOPES, auth::BOT_SCOPES] {
        assert!(scopes.contains(&"files:read") && scopes.contains(&"files:write"));
    }
}

mod draft_target {
    use super::*;
    use crate::test_http::MockServer;
    use serde_json::json;

    fn server(ims: serde_json::Value) -> MockServer {
        MockServer::start(move |req| {
            let body = match req.path() {
                "/users.info" => json!({"ok": true, "user": {"id": "U0000000A", "name": "kb"}}),
                "/users.conversations" => json!({"ok": true, "channels": ims}),
                "/conversations.open" => json!({"ok": true, "channel": {"id": "DNEW00001"}}),
                other => json!({"ok": false, "error": format!("unexpected {other}")}),
            };
            (200, body.to_string())
        })
    }

    fn slack(server: &MockServer) -> crate::slack::Slack {
        crate::slack::Slack::with_base(MockServer::client(), &server.base, "xoxp-test".into())
    }

    #[tokio::test]
    async fn existing_dm_only_uses_the_dm_already_there() {
        let s = server(json!([{"id": "DOTHER001", "user": "U0000000Z"},
                              {"id": "DKB000001", "user": "U0000000A"}]));
        assert_eq!(
            draft_channel(&slack(&s), "U0000000A", true).await.unwrap(),
            "DKB000001"
        );
        assert!(
            s.requests()
                .iter()
                .all(|r| r.path() != "/conversations.open"),
            "nothing was opened"
        );
    }

    #[tokio::test]
    async fn existing_dm_only_refuses_when_there_is_none() {
        let s = server(json!([{"id": "DOTHER001", "user": "U0000000Z"}]));
        let err = draft_channel(&slack(&s), "U0000000A", true)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("no DM with U0000000A"), "{err}");
        assert!(
            s.requests()
                .iter()
                .all(|r| r.path() != "/conversations.open")
        );
    }

    #[tokio::test]
    async fn without_the_flag_a_person_gets_their_dm_opened() {
        let s = server(json!([]));
        assert_eq!(
            draft_channel(&slack(&s), "U0000000A", false).await.unwrap(),
            "DNEW00001"
        );
        assert!(
            s.requests()
                .iter()
                .any(|r| r.path() == "/conversations.open")
        );
    }

    #[tokio::test]
    async fn the_flag_changes_nothing_for_a_channel() {
        let s = server(json!([]));
        assert_eq!(
            draft_channel(&slack(&s), "C0123ABCD", true).await.unwrap(),
            "C0123ABCD"
        );
        assert!(s.requests().is_empty());
    }
}

#[test]
fn a_search_limit_over_a_page_is_called_out() {
    assert_eq!(search_cap_note(100), None);
    let note = search_cap_note(500).unwrap();
    assert!(
        note.contains("at most 100") && note.contains("not 500"),
        "{note}"
    );
}

mod shown_times {
    use super::*;
    use crate::test_http::MockServer;
    use serde_json::json;

    async fn read_channel(zone: Zone) -> String {
        let server = MockServer::start(|req| {
            let body = match req.path() {
                "/conversations.history" => json!({"ok": true, "messages": [
                    {"ts": "1789357009.709100", "user": "U0000000A", "text": "deployed"}
                ]}),
                "/users.info" => json!({"ok": true, "user": {"id": "U0000000A", "name": "kb"}}),
                other => json!({"ok": false, "error": format!("unexpected {other}")}),
            };
            (200, body.to_string())
        });
        let slack =
            crate::slack::Slack::with_base(MockServer::client(), &server.base, "xoxp-test".into());
        let token = auth::TokenRef {
            key: "SLACK_T".into(),
            kind: auth::Kind::User,
            team: String::new(),
            team_id: String::new(),
            account: String::new(),
            url: String::new(),
            client_id_key: None,
            client_secret_key: None,
        };
        let mut ctx = AppContext::new().unwrap();
        let rest = v(&["C0123ABCD"]);
        api_command(&mut ctx, &slack, &token, "read", &rest, &rest, zone)
            .await
            .unwrap();
        std::mem::take(&mut ctx.output)
    }

    #[tokio::test]
    async fn slack_read_shows_utc_by_default_and_local_on_request() {
        let utc = read_channel(Zone::Utc).await;
        // The ts stays as the id it is; the time beside it is ISO UTC.
        assert_eq!(
            utc,
            "1789357009.709100\t2026-09-14T03:36:49Z\tkb\tdeployed\n"
        );
        let local = read_channel(Zone::Local).await;
        let shown = crate::timefmt::from_epoch(1_789_357_009, Zone::Local);
        assert_eq!(local, format!("1789357009.709100\t{shown}\tkb\tdeployed\n"));
    }
}
