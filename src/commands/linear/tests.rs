use super::*;

fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn field_flags_become_changes() {
    let c = changes_from(&v(&[
        "--state",
        "In Review",
        "--priority",
        "high",
        "--labels",
        "bug, ui",
        "--add-label",
        "p1",
        "--add-label",
        "regression",
        "--unassign",
        "--estimate",
        "3",
    ]))
    .unwrap();
    assert_eq!(c.state.as_deref(), Some("In Review"));
    assert_eq!(c.priority, Some(2));
    assert_eq!(c.labels, Some(vec!["bug".to_string(), "ui".to_string()]));
    assert_eq!(c.add_labels, vec!["p1", "regression"]);
    assert!(c.unassign);
    assert_eq!(c.estimate, Some(3));
    assert!(!c.is_empty());
    assert!(changes_from(&v(&[])).unwrap().is_empty());
}

#[test]
fn bad_field_values_are_refused() {
    assert!(changes_from(&v(&["--priority", "asap"])).is_err());
    assert!(changes_from(&v(&["--estimate", "three"])).is_err());
    assert!(changes_from(&v(&["--description", "a", "--description-file", "b"])).is_err());
}

#[test]
fn a_switch_does_not_eat_the_identifier() {
    let a = v(&["--unassign", "ENG-1", "--state", "Done"]);
    assert_eq!(positional_with_switches(&a, SWITCHES), vec!["ENG-1"]);
}

#[test]
fn every_usage_line_names_a_real_subcommand() {
    for verb in [
        "setup",
        "add",
        "login",
        "accounts",
        "use",
        "status",
        "doctor",
        "logout",
        "issues",
        "mine",
        "issue",
        "teams",
        "projects",
        "cycles",
        "create",
        "update",
        "comment",
        "history",
        "activity",
        "inbox",
        "workspaces",
        "attachments",
        "download",
        "upload",
        "link",
    ] {
        let tokens: Vec<&str> = USAGE
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
            .collect();
        assert!(tokens.contains(&verb), "usage is missing {verb}");
    }
}

#[test]
fn the_walkthrough_names_the_callback_and_keys() {
    let w = setup_walkthrough(53695, "LIN_T", "LIN_ID", "LIN_SECRET");
    assert!(w.contains("Callback URL: http://localhost:53695/callback"));
    assert!(w.contains("sidekar linear add --token LIN_T"));
    assert!(w.contains("--client-id LIN_ID --client-secret LIN_SECRET"));
}

#[test]
fn times_are_shortened_to_the_minute() {
    assert_eq!(short_time("2026-10-10T14:03:05.123Z"), "2026-10-10 14:03");
    assert_eq!(short_time("bad"), "bad");
}

#[test]
fn inbox_lines_lead_with_the_id_and_unread_marker() {
    let n = api::Notification {
        id: "n1".into(),
        kind: "issueComment".into(),
        created: "2026-10-10T14:03:05Z".into(),
        actor: "ann".into(),
        issue: "ENG-1".into(),
        issue_title: "Bug".into(),
        comment: "looks good".into(),
        ..Default::default()
    };
    assert_eq!(
        api_inbox_line(&n),
        "n1\tUNREAD\t2026-10-10 14:03\tissueComment\tann\tENG-1 Bug\t“looks good”"
    );
    let read = api::Notification {
        read: true,
        project: "Web".into(),
        ..n.clone()
    };
    assert!(api_inbox_line(&read).contains("\tread\t"));
}

#[test]
fn inbox_switches_do_not_swallow_ids() {
    let pos = positional_with_switches(&v(&["read", "--unread", "id1", "id2"]), SWITCHES);
    assert_eq!(pos, ["read", "id1", "id2"]);
}

#[test]
fn attach_values_are_not_positionals() {
    let pos = positional_with_switches(
        &v(&["ENG-1", "--attach", "a.png", "--body", "hi", "--print"]),
        SWITCHES,
    );
    assert_eq!(pos, ["ENG-1"]);
}
