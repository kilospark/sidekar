// Pane-to-pid parsing moved to `bus::presence::pid_of_pane`, which defines the
// pane formats; its tests live there.
use serde_json::json;

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
