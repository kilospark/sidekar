use super::*;

/// Kill every other `sidekar daemon start` process plus orphan relaunch helpers.
///
/// Called at startup and periodically from the housekeeping loop. Pidfile-based
/// cleanup alone is not enough: a stale daemon may outlive its pidfile entry
/// (e.g. SIGTERM-on-shutdown got stuck in `deregister_discover_port`), keeping
/// port 21517 bound and siphoning the extension's WebSocket to a dead daemon.
pub(super) fn kill_orphaned_daemons() {
    let my_pid = std::process::id() as i32;
    kill_other_sidekar_daemons(my_pid);
    kill_orphan_relaunch_helpers(my_pid);
}

fn kill_other_sidekar_daemons(my_pid: i32) {
    let pids = find_other_sidekar_daemon_pids(my_pid);
    if pids.is_empty() {
        return;
    }

    for pid in &pids {
        unsafe {
            libc::kill(*pid, libc::SIGTERM);
        }
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let survivors: Vec<i32> = pids.iter().copied().filter(|p| pid_alive(*p)).collect();
        if survivors.is_empty() {
            return;
        }
        if std::time::Instant::now() >= deadline {
            for pid in survivors {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
            return;
        }
    }
}

fn kill_orphan_relaunch_helpers(my_pid: i32) {
    let Ok(output) = std::process::Command::new("pgrep")
        .args(["-f", "sidekar daemon relaunch"])
        .output()
    else {
        return;
    };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Ok(pid) = line.trim().parse::<i32>()
            && pid != my_pid
        {
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
    }
}

fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Enumerate `sidekar daemon start` processes by scanning `ps` output.
///
/// Matching on argv (not substring) avoids false positives from agent processes
/// whose prompt text happens to contain the literal "sidekar daemon start".
fn find_other_sidekar_daemon_pids(my_pid: i32) -> Vec<i32> {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-Ao", "pid=,args="])
        .output()
    else {
        return Vec::new();
    };
    let mut pids = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let line = line.trim_start();
        let Some((pid_str, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<i32>() else {
            continue;
        };
        if pid == my_pid {
            continue;
        }
        if is_sidekar_daemon_start(rest.trim()) {
            pids.push(pid);
        }
    }
    pids
}

fn is_sidekar_daemon_start(cmdline: &str) -> bool {
    let mut parts = cmdline.split_whitespace();
    let Some(exe) = parts.next() else {
        return false;
    };
    let basename = std::path::Path::new(exe)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if basename != "sidekar" {
        return false;
    }
    let args: Vec<&str> = parts.collect();
    matches!(args.as_slice(), ["daemon", "start", ..])
}

const SWEEP_INTERVAL_SECS: u64 = 60;
const UPDATE_CHECK_INTERVAL_SECS: u64 = 3600;
const STALE_MESSAGE_AGE_SECS: u64 = 3600;
const DB_MAINTENANCE_INTERVAL_SECS: u64 = 900; // 15 min: WAL checkpoint
const DB_VACUUM_INTERVAL_SECS: u64 = 86_400; // 24 h: VACUUM + event purge
const STALE_EVENT_AGE_SECS: u64 = 7 * 86_400; // keep 7 days of events
const STALE_WATCH_AGE_SECS: u64 = 48 * 3600; // zombie-watch TTL

pub(super) async fn housekeeping_loop(http_port: u16, ext_state: crate::ext::SharedExtState) {
    let mut sweep_interval =
        tokio::time::interval(std::time::Duration::from_secs(SWEEP_INTERVAL_SECS));
    let mut update_interval =
        tokio::time::interval(std::time::Duration::from_secs(UPDATE_CHECK_INTERVAL_SECS));
    let mut db_maint_interval =
        tokio::time::interval(std::time::Duration::from_secs(DB_MAINTENANCE_INTERVAL_SECS));
    let mut db_vacuum_interval =
        tokio::time::interval(std::time::Duration::from_secs(DB_VACUUM_INTERVAL_SECS));

    sweep_interval.tick().await;
    update_interval.tick().await;
    db_maint_interval.tick().await;
    db_vacuum_interval.tick().await;
    if http_port > 0 {
        discover_heartbeat(http_port).await;
    }

    loop {
        tokio::select! {
            _ = sweep_interval.tick() => {
                kill_orphaned_daemons();
                sweep_dead_agents();
                // Before the reaper, never after: the reaper deletes undelivered
                // mail an hour old, and doing that first would lose it in silence
                // instead of telling its senders.
                settle_orphaned_mail();
                // A session host that crashed leaves its socket and a
                // "running" meta behind; mark it ended.
                let _ = crate::hosted::reap_all();
                cleanup_stale_messages();
                crate::ext::sweep_stale_watches(&ext_state, STALE_WATCH_AGE_SECS).await;
                crate::ext::sweep_stale_tab_monitors(&ext_state, STALE_WATCH_AGE_SECS).await;
                if http_port > 0 {
                    discover_heartbeat(http_port).await;
                }
            }
            _ = db_maint_interval.tick() => {
                let _ = crate::broker::wal_checkpoint_truncate();
            }
            _ = db_vacuum_interval.tick() => {
                let _ = crate::broker::cleanup_old_events(STALE_EVENT_AGE_SECS);
                let _ = crate::broker::vacuum_db();
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

/// Sweep dead agents from the broker: unregister any whose process is gone.
///
/// This is the crash path. An agent that exits normally settles its own mail on
/// the way out; one that was killed never got the chance, so its mail is settled
/// here — before the name is freed, so the next agent to take the name does not
/// inherit it.
fn sweep_dead_agents() {
    let agents = match crate::broker::list_agents(None) {
        Ok(a) => a,
        Err(_) => return,
    };
    let now = crate::message::epoch_secs();
    for agent in agents {
        let dead = agent
            .id
            .pane
            .as_deref()
            .and_then(crate::bus::presence::pid_of_pane)
            .is_some_and(|pid| !crate::bus::presence::process_alive(pid));
        if dead {
            let nick = agent.id.nick.as_deref().unwrap_or_default();
            let _ = crate::broker::bounce_mail_for_departed(&agent.id.name, nick, now);
            let _ = crate::broker::unregister_agent(&agent.id.name);
        }
    }
}

/// How long mail may wait on a name nobody holds before it is settled.
///
/// Long enough that an agent re-registering under its own name is never caught
/// in the gap; the same as the sweep interval, so nothing waits more than two.
const ORPHANED_MAIL_GRACE_SECS: u64 = 60;

/// Settle mail waiting on names that nobody holds.
///
/// The backstop for every way an agent can leave without settling its own mail —
/// in particular, an agent still running an older sidekar, which exits the old
/// way and leaves its mail queued on a name that is now free.
fn settle_orphaned_mail() {
    let now = crate::message::epoch_secs();
    let Ok(names) = crate::broker::orphaned_mail_recipients(now, ORPHANED_MAIL_GRACE_SECS) else {
        return;
    };
    for name in names {
        let nick = crate::broker::last_known_nick(&name).unwrap_or_default();
        let _ = crate::broker::bounce_mail_for_departed(&name, &nick, now);
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
            crate::broker::try_log_event(
                "info",
                "updater",
                &format!("update v{latest} available, installing in background"),
                None,
            );
            if let Err(e) = crate::api_client::self_update(&latest).await {
                crate::broker::try_log_error(
                    "updater",
                    "background update failed",
                    Some(&format!("{e:#}")),
                );
            } else {
                crate::broker::try_log_event(
                    "info",
                    "updater",
                    &format!("updated to v{latest}; restarting daemon"),
                    None,
                );
                if let Err(e) = restart_current_process() {
                    crate::broker::try_log_error(
                        "updater",
                        "updated, but failed to restart daemon",
                        Some(&format!("{e:#}")),
                    );
                }
            }
        }
        Ok(None) => {}
        Err(e) => {
            // Log the check failure so a broken /v1/version endpoint
            // (e.g. Vercel serving stale data, auth expired, TLS
            // issue) surfaces through `sidekar monitor` instead of
            // quietly skipping every background update attempt.
            // Rate-limited upstream by should_check_for_update()'s
            // UPDATE_CHECK_INTERVAL_SECS throttle, so this won't
            // fill the log with transient network errors.
            crate::broker::try_log_error(
                "updater",
                "background update check failed",
                Some(&format!("{e:#}")),
            );
        }
    }
}

pub(super) async fn discover_heartbeat(port: u16) {
    if crate::auth::auth_token().is_none() {
        return;
    }
    crate::api_client::deregister_discover_port().await;
    if let Err(e) = crate::api_client::register_discover_port(port).await {
        crate::broker::try_log_error("discover", "heartbeat failed", Some(&format!("{e:#}")));
    }
}
