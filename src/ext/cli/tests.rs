use super::build_command;

#[test]
fn ext_read_requires_explicit_tab() {
    let err = build_command("read", &[], None).unwrap_err().to_string();
    assert!(err.contains("requires an explicit tab ID"));
}

#[test]
fn ext_click_uses_global_tab_override() {
    let cmd = build_command("click", &[String::from("text:OK")], Some(42)).unwrap();
    assert_eq!(cmd.get("tabId").and_then(|v| v.as_u64()), Some(42));
}

#[test]
fn ext_navigate_accepts_positional_tab() {
    let cmd = build_command(
        "navigate",
        &[String::from("https://example.com"), String::from("77")],
        None,
    )
    .unwrap();
    assert_eq!(cmd.get("tabId").and_then(|v| v.as_u64()), Some(77));
}

#[test]
fn ext_tabs_does_not_require_tab() {
    let cmd = build_command("tabs", &[], None).unwrap();
    assert_eq!(cmd.get("command").and_then(|v| v.as_str()), Some("tabs"));
    assert!(cmd.get("tabId").is_none());
}
