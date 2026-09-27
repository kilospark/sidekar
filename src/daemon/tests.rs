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

#[test]
fn a_cargo_build_is_not_left_running_as_the_daemon() {
    use crate::daemon::is_cargo_build;
    use std::path::Path;
    assert!(is_cargo_build(Path::new(
        "/Users/me/src/sidekar/target/release/sidekar"
    )));
    assert!(is_cargo_build(Path::new(
        "/Users/me/src/sidekar/target/debug/sidekar"
    )));
    assert!(!is_cargo_build(Path::new("/Users/me/.cargo/bin/sidekar")));
    assert!(!is_cargo_build(Path::new("/usr/local/bin/sidekar")));
}
