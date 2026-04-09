use crate::daemon::housekeeping::pid_from_agent_pane;

#[test]
fn pid_from_agent_pane_recognizes_local_agent_prefixes() {
    assert_eq!(pid_from_agent_pane("pty-123"), Some(123));
    assert_eq!(pid_from_agent_pane("repl-456"), Some(456));
    assert_eq!(pid_from_agent_pane("cli-789"), Some(789));
}

#[test]
fn pid_from_agent_pane_rejects_non_pid_panes() {
    assert_eq!(pid_from_agent_pane("tab-123"), None);
    assert_eq!(pid_from_agent_pane("repl-abc"), None);
    assert_eq!(pid_from_agent_pane("pty-"), None);
    assert_eq!(pid_from_agent_pane(""), None);
}
