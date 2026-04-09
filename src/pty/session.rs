use super::*;

/// Kill and reap a child process, clean up broker and socket state.
pub(crate) fn cleanup_child_and_state(
    child_pid: libc::pid_t,
    agent_name: Option<&str>,
    socket_path: Option<&std::path::Path>,
) {
    // Kill child if still running
    if unsafe { libc::kill(child_pid, 0) } == 0 {
        unsafe { libc::kill(child_pid, libc::SIGTERM) };
        // Brief wait, then force kill
        std::thread::sleep(std::time::Duration::from_millis(500));
        if unsafe { libc::kill(child_pid, 0) } == 0 {
            unsafe { libc::kill(child_pid, libc::SIGKILL) };
        }
    }
    // Reap
    let mut status: libc::c_int = 0;
    unsafe { libc::waitpid(child_pid, &mut status, libc::WNOHANG) };

    // Clean broker
    if let Some(name) = agent_name {
        let _ = broker::unregister_agent(name);
    }
    // Clean socket
    if let Some(path) = socket_path {
        let _ = std::fs::remove_file(path);
    }
}

pub(crate) fn resolved_relay_policy(override_policy: Option<bool>) -> crate::config::RelayMode {
    match override_policy {
        Some(true) => crate::config::RelayMode::On,
        Some(false) => crate::config::RelayMode::Off,
        None => crate::config::relay_mode(),
    }
}

pub(crate) fn relay_policy_label(override_policy: Option<bool>) -> String {
    match override_policy {
        Some(true) => "--relay".to_string(),
        Some(false) => "--no-relay".to_string(),
        None => format!("config:{}", crate::config::relay_mode().as_str()),
    }
}

pub(crate) async fn connect_relay_tunnel(
    token: &str,
    session_name: &str,
    agent_type: &str,
    cwd: &str,
    nick: &str,
) -> Result<(crate::tunnel::TunnelSender, crate::tunnel::TunnelReceiver)> {
    let (cols, rows) = current_terminal_size().unwrap_or((80, 24));
    crate::tunnel::connect(token, session_name, agent_type, cwd, nick, cols, rows).await
}
