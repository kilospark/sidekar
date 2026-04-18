//! Bus message poller — reads from the SQLite bus_queue and delivers
//! messages to the local agent via PTY write.

use crate::broker;
use crate::transport::{Broker as BrokerTransport, RelayHttp, Transport};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static POLLER_SHUTDOWN: AtomicBool = AtomicBool::new(false);

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const CLEANUP_INTERVAL_POLLS: u32 = 120; // clean old messages every 60s (120 * 500ms)
const NUDGE_INTERVAL_POLLS: u32 = 120; // check nudges every 60s
const MAX_MESSAGE_AGE_SECS: u64 = 3600;
const NUDGE_SCHEDULE_SECS: [u64; 5] = [60, 120, 300, 600, 900];
const NUDGE_MAX: u32 = 5;
const USER_IDLE_BEFORE_INJECT: Duration = Duration::from_millis(1000);
const INJECT_CHECK_INTERVAL: Duration = Duration::from_millis(100);

pub struct UserInputState {
    last_user_input_at_ms: std::sync::atomic::AtomicU64,
    pending_line: Mutex<Vec<u8>>,
}

impl UserInputState {
    pub fn new() -> Self {
        Self {
            last_user_input_at_ms: std::sync::atomic::AtomicU64::new(0),
            pending_line: Mutex::new(Vec::new()),
        }
    }

    pub fn mark_activity(&self) {
        self.last_user_input_at_ms
            .store(epoch_millis(), Ordering::Relaxed);
    }

    pub fn set_pending_line(&self, line: &[u8]) {
        if let Ok(mut pending) = self.pending_line.lock() {
            pending.clear();
            pending.extend_from_slice(line);
        }
    }

    pub fn clear_pending_line(&self) {
        if let Ok(mut pending) = self.pending_line.lock() {
            pending.clear();
        }
    }

    pub fn has_pending_line(&self) -> bool {
        self.pending_line
            .lock()
            .map(|pending| !pending.is_empty())
            .unwrap_or(false)
    }

    pub fn is_idle(&self) -> bool {
        let last = self.last_user_input_at_ms.load(Ordering::Relaxed);
        if last == 0 {
            return true;
        }
        epoch_millis().saturating_sub(last) >= USER_IDLE_BEFORE_INJECT.as_millis() as u64
    }
}

impl Default for UserInputState {
    fn default() -> Self {
        Self::new()
    }
}

/// Signal all background workers started by this module to stop.
pub fn shutdown_poller() {
    POLLER_SHUTDOWN.store(true, Ordering::Relaxed);
}

/// Start the full PTY poller: an inbound thread that delivers bus messages
/// into the wrapped agent's PTY, plus the shared nudge+cleanup sweep.
pub fn start_poller(
    agent_name: String,
    pty_fd: Arc<OwnedFd>,
    input_state: Arc<UserInputState>,
    child_pid: i32,
) {
    POLLER_SHUTDOWN.store(false, Ordering::Relaxed);

    let inject_agent = agent_name.clone();
    std::thread::spawn(move || {
        loop {
            if POLLER_SHUTDOWN.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(POLL_INTERVAL);
            if let Ok(messages) = broker::poll_messages(&inject_agent) {
                for msg in messages {
                    deliver_to_pty(&pty_fd, &input_state, &msg.body, child_pid);
                }
            }
        }
    });

    start_nudger(agent_name);
}

/// Start the nudge + cleanup sweep for this agent. No PTY delivery — use this
/// from embeds like the REPL that handle inbound messages on their own path.
/// Stopped by `shutdown_poller`.
pub fn start_nudger(agent_name: String) {
    POLLER_SHUTDOWN.store(false, Ordering::Relaxed);

    std::thread::spawn(move || {
        let mut cleanup_poll_count: u32 = 0;
        let mut nudge_poll_count: u32 = 0;

        loop {
            if POLLER_SHUTDOWN.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(POLL_INTERVAL);

            cleanup_poll_count += 1;
            if cleanup_poll_count >= CLEANUP_INTERVAL_POLLS {
                cleanup_poll_count = 0;
                let _ = broker::cleanup_old_messages(MAX_MESSAGE_AGE_SECS);
            }

            nudge_poll_count += 1;
            if nudge_poll_count >= NUDGE_INTERVAL_POLLS {
                nudge_poll_count = 0;
                send_nudges(&agent_name);
            }
        }
    });
}

/// Send nudges for this agent's unanswered outbound requests.
fn send_nudges(agent_name: &str) {
    let requests = match broker::outbound_for_sender(agent_name) {
        Ok(r) => r,
        Err(_) => return,
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    for request in requests {
        let wait_secs = NUDGE_SCHEDULE_SECS
            .get(request.nudge_count as usize)
            .copied()
            .unwrap_or(*NUDGE_SCHEDULE_SECS.last().unwrap_or(&900));
        let last_event_at = request.last_nudged_at.unwrap_or(request.created_at);
        let elapsed_since_last_event = now.saturating_sub(last_event_at);

        if elapsed_since_last_event < wait_secs {
            continue;
        }

        // Check if we've hit max nudges
        if request.nudge_count >= NUDGE_MAX {
            continue;
        }

        // Check if the pending message still exists (hasn't been answered)
        if broker::pending_message(&request.msg_id)
            .ok()
            .flatten()
            .is_none()
        {
            let _ = broker::delete_outbound_request(&request.msg_id);
            continue;
        }

        // Check if recipient is still alive
        if !is_recipient_alive(&request.recipient_name) {
            let _ = broker::delete_outbound_request(&request.msg_id);
            let _ = broker::clear_pending(&request.msg_id);
            continue;
        }

        // Send the nudge
        let nudge_msg = format!(
            "[sidekar] You have an unanswered request from {}. Reply using bus send or bus done with --reply-to={}",
            request.sender_label, request.msg_id
        );

        let delivery_result = match request.transport_name.as_str() {
            "broker" => BrokerTransport.deliver(&request.transport_target, &nudge_msg, "sidekar"),
            "relay_http" => RelayHttp.deliver(&request.transport_target, &nudge_msg, "sidekar"),
            _ => continue,
        };

        if delivery_result.is_ok() {
            let _ = broker::increment_nudge_count(&request.msg_id, now);
        }
    }
}

/// Check if the recipient agent is still registered and alive.
fn is_recipient_alive(recipient_name: &str) -> bool {
    let agent = match broker::find_agent(recipient_name, None) {
        Ok(Some(a)) => a,
        _ => return false,
    };

    if let Some(ref pane) = agent.id.pane
        && let Some(pid_str) = pane.strip_prefix("pty-")
        && let Ok(pid) = pid_str.parse::<i32>()
    {
        return unsafe { libc::kill(pid, 0) } == 0;
    }

    // If we can't determine PID, assume alive (could be a relay agent)
    true
}

fn deliver_to_pty(fd: &OwnedFd, input_state: &UserInputState, message: &str, child_pid: i32) {
    let raw_fd = fd.as_raw_fd();

    // Do not inject while the user is actively typing or has a pending line.
    let mut waited = 0u32;
    while !input_state.is_idle() || input_state.has_pending_line() {
        if POLLER_SHUTDOWN.load(Ordering::Relaxed) {
            return;
        }
        waited += 1;
        if waited.is_multiple_of(50) {
            // Log every 5s while blocked (50 * 100ms)
            crate::broker::try_log_event(
                "debug",
                "poller",
                &format!(
                    "inject blocked: idle={} pending_line={} waited={}s msg_len={}",
                    input_state.is_idle(),
                    input_state.has_pending_line(),
                    waited / 10,
                    message.len(),
                ),
                None,
            );
        }
        std::thread::sleep(INJECT_CHECK_INTERVAL);
    }

    // Write message text
    if let Err(e) = write_all_raw(raw_fd, message.as_bytes()) {
        crate::broker::try_log_event(
            "error",
            "poller",
            &format!("inject write failed: {e}"),
            None,
        );
        return;
    }
    // Brief pause then send Enter (CR)
    std::thread::sleep(Duration::from_millis(150));
    if let Err(e) = write_all_raw(raw_fd, b"\r") {
        crate::broker::try_log_event(
            "error",
            "poller",
            &format!("inject CR write failed: {e}"),
            None,
        );
        return;
    }

    // Wake up the agent's event loop. Some agents (Claude Code / Node.js)
    // don't notice new bytes in the PTY slave input buffer until an event
    // kicks their I/O loop. SIGWINCH is harmless — the agent re-queries
    // terminal size (a no-op) and processes pending stdin in the same cycle.
    unsafe { libc::kill(child_pid, libc::SIGWINCH) };

    crate::broker::try_log_event(
        "debug",
        "poller",
        &format!(
            "injected {}B + CR + SIGWINCH (waited={}s)",
            message.len(),
            waited / 10,
        ),
        None,
    );
}

fn write_all_raw(fd: i32, mut buf: &[u8]) -> anyhow::Result<()> {
    while !buf.is_empty() {
        let n = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
        if n > 0 {
            buf = &buf[n as usize..];
        } else if n == 0 {
            anyhow::bail!("write returned 0");
        } else {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if err.kind() == std::io::ErrorKind::WouldBlock {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            anyhow::bail!("write failed: {err}");
        }
    }
    Ok(())
}

fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
