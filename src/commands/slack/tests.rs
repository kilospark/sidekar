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
