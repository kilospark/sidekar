use super::*;

/// Kill the previously registered daemon (if any) and any orphaned relaunch helpers.
pub(super) fn kill_orphaned_daemons() {
    let my_pid = std::process::id() as i32;

    if let Some(old_pid) = get_pid()
        && old_pid != my_pid
    {
        unsafe {
            libc::kill(old_pid, libc::SIGTERM);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    if let Ok(output) = std::process::Command::new("pgrep")
        .args(["-f", "sidekar daemon relaunch"])
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if let Ok(pid) = line.trim().parse::<i32>()
                && pid != my_pid
            {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
        }
    }
}

const SWEEP_INTERVAL_SECS: u64 = 60;
const UPDATE_CHECK_INTERVAL_SECS: u64 = 3600;
const STALE_MESSAGE_AGE_SECS: u64 = 3600;

pub(super) async fn housekeeping_loop(http_port: u16) {
    let mut sweep_interval =
        tokio::time::interval(std::time::Duration::from_secs(SWEEP_INTERVAL_SECS));
    let mut update_interval =
        tokio::time::interval(std::time::Duration::from_secs(UPDATE_CHECK_INTERVAL_SECS));

    sweep_interval.tick().await;
    update_interval.tick().await;
    if http_port > 0 {
        discover_heartbeat(http_port).await;
    }

    loop {
        tokio::select! {
            _ = sweep_interval.tick() => {
                sweep_dead_agents();
                cleanup_stale_messages();
                if http_port > 0 {
                    discover_heartbeat(http_port).await;
                }
            }
            _ = update_interval.tick() => {
                check_for_update().await;
            }
        }
    }
}

/// Periodically reap idle CDP connections from the pool.
pub(super) async fn cdp_pool_reaper(pool: Arc<Mutex<crate::cdp_proxy::CdpPool>>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
    interval.tick().await;
    loop {
        interval.tick().await;
        pool.lock().await.reap_idle();
    }
}

/// Extract a local process PID from broker pane IDs that encode one.
pub(super) fn pid_from_agent_pane(pane: &str) -> Option<i32> {
    for prefix in ["pty-", "repl-", "cli-"] {
        if let Some(pid_str) = pane.strip_prefix(prefix)
            && let Ok(pid) = pid_str.parse::<i32>()
        {
            return Some(pid);
        }
    }
    None
}

/// Sweep dead agents from the broker. Checks each local agent PID encoded in
/// the pane ID and unregisters agents whose process is no longer alive.
fn sweep_dead_agents() {
    let agents = match crate::broker::list_agents(None) {
        Ok(a) => a,
        Err(_) => return,
    };
    for agent in agents {
        if let Some(ref pane) = agent.id.pane
            && let Some(pid) = pid_from_agent_pane(pane)
            && unsafe { libc::kill(pid, 0) } != 0
        {
            let _ = crate::broker::unregister_agent(&agent.id.name);
        }
    }
}

/// Clean up stale messages older than STALE_MESSAGE_AGE_SECS.
fn cleanup_stale_messages() {
    let _ = crate::broker::cleanup_old_messages(STALE_MESSAGE_AGE_SECS);
    let _ = crate::broker::cleanup_old_pending_requests(STALE_MESSAGE_AGE_SECS);
    let _ = crate::broker::cleanup_old_outbound_requests(STALE_MESSAGE_AGE_SECS);
}

/// Check for updates and install in background.
async fn check_for_update() {
    if !crate::config::load_config().auto_update {
        return;
    }
    if !crate::api_client::should_check_for_update() {
        return;
    }
    match crate::api_client::check_for_update().await {
        Ok(Some(latest)) => {
            eprintln!("sidekar: update v{latest} available, installing in background...");
            if let Err(e) = crate::api_client::self_update(&latest).await {
                eprintln!("sidekar: background update failed: {e:#}");
            } else {
                eprintln!("sidekar: updated to v{latest}; restarting daemon...");
                if let Err(e) = restart_current_process() {
                    eprintln!("sidekar: updated, but failed to restart daemon: {e:#}");
                }
            }
        }
        Ok(None) => {}
        Err(_) => {}
    }
}

pub(super) async fn discover_heartbeat(port: u16) {
    if crate::auth::auth_token().is_none() {
        return;
    }
    crate::api_client::deregister_discover_port().await;
    if let Err(e) = crate::api_client::register_discover_port(port).await
        && crate::runtime::verbose()
    {
        eprintln!("sidekar: discover heartbeat failed: {e:#}");
    }
}
