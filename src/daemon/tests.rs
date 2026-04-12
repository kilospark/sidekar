use crate::daemon::housekeeping::pid_from_agent_pane;
use serde_json::json;

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

#[tokio::test]
async fn ping_reports_daemon_pid() {
    let state = std::sync::Arc::new(tokio::sync::Mutex::new(super::DaemonState::new()));
    let response = super::command::handle_command(&json!({"type": "ping"}), &state).await;
    assert_eq!(response.get("pong").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        response.get("pid").and_then(|v| v.as_u64()),
        Some(std::process::id() as u64)
    );
}
