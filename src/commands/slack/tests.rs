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
