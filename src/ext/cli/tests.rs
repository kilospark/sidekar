use super::build_command;

#[test]
fn ext_read_requires_explicit_tab() {
    let err = build_command("read", &[], None, false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("requires an explicit tab ID"));
}

#[test]
fn ext_click_uses_global_tab_override() {
    let cmd = build_command("click", &[String::from("text:OK")], Some(42), false).unwrap();
    assert_eq!(cmd.get("tabId").and_then(|v| v.as_u64()), Some(42));
}

#[test]
fn ext_navigate_accepts_positional_tab() {
    let cmd = build_command(
        "navigate",
        &[String::from("https://example.com"), String::from("77")],
        None,
        false,
    )
    .unwrap();
    assert_eq!(cmd.get("tabId").and_then(|v| v.as_u64()), Some(77));
}

#[test]
fn ext_tabs_does_not_require_tab() {
    let cmd = build_command("tabs", &[], None, false).unwrap();
    assert_eq!(cmd.get("command").and_then(|v| v.as_str()), Some("tabs"));
    assert!(cmd.get("tabId").is_none());
}

#[test]
fn ext_new_tab_focus_sets_json_flag() {
    let cmd = build_command(
        "new-tab",
        &[String::from("https://example.com")],
        None,
        true,
    )
    .unwrap();
    assert_eq!(cmd.get("focus").and_then(|v| v.as_bool()), Some(true));
}

#[test]
fn ext_new_tab_default_no_focus_flag() {
    let cmd = build_command(
        "new-tab",
        &[String::from("https://example.com")],
        None,
        false,
    )
    .unwrap();
    assert!(cmd.get("focus").is_none());
}

#[test]
fn ext_key_needs_a_key_rather_than_assuming_one() {
    // An implicit keystroke is the wrong default: pressing something the caller
    // did not name can submit a form or dismiss a dialog.
    let err = build_command("key", &[], Some(7), false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("Usage: sidekar browser ext key"));
}

#[test]
fn ext_key_sends_to_the_focused_element_when_no_target_is_named() {
    let args: Vec<String> = vec!["Enter".to_string()];
    let cmd = build_command("key", &args, Some(7), false).unwrap();
    assert_eq!(cmd["command"], "key");
    assert_eq!(cmd["key"], "Enter");
    assert_eq!(cmd["tabId"], 7);
    // No selector: the event goes wherever the caret already is, which is what
    // "type into the box, then press Enter" needs.
    assert!(cmd.get("selector").is_none());
    for m in ["ctrl", "shift", "alt", "meta"] {
        assert_eq!(cmd[m], false, "{m} should default off");
    }
}

#[test]
fn ext_key_takes_a_key_a_target_and_modifiers() {
    let args: Vec<String> = ["Tab", "39", "--shift"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let cmd = build_command("key", &args, Some(3), false).unwrap();
    assert_eq!(cmd["key"], "Tab");
    assert_eq!(cmd["selector"], "39");
    assert_eq!(cmd["shift"], true);
    assert_eq!(cmd["ctrl"], false);
}

#[test]
fn ext_key_does_not_read_a_modifier_as_the_target() {
    let args: Vec<String> = ["Enter", "--ctrl"].iter().map(|s| s.to_string()).collect();
    let cmd = build_command("key", &args, Some(1), false).unwrap();
    assert_eq!(cmd["key"], "Enter");
    assert_eq!(cmd["ctrl"], true);
    assert!(cmd.get("selector").is_none(), "--ctrl is not a selector");
}

#[test]
fn ext_press_is_accepted_as_an_alias() {
    let args: Vec<String> = vec!["Escape".to_string()];
    let cmd = build_command("press", &args, Some(2), false).unwrap();
    assert_eq!(cmd["command"], "key");
    assert_eq!(cmd["key"], "Escape");
}
