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

    /// Returns true if the user has typed anything since the PTY started.
    pub fn has_ever_had_input(&self) -> bool {
        self.last_user_input_at_ms.load(Ordering::Relaxed) > 0
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

/// Signal the poller to stop.
pub fn shutdown_poller() {
    POLLER_SHUTDOWN.store(true, Ordering::Relaxed);
}

/// Start the background poller thread. Returns immediately.
pub fn start_poller(agent_name: String, pty_fd: Arc<OwnedFd>, input_state: Arc<UserInputState>) {
    POLLER_SHUTDOWN.store(false, Ordering::Relaxed);

    std::thread::spawn(move || {
        let mut poll_count: u32 = 0;
        let mut nudge_poll_count: u32 = 0;

        loop {
            if POLLER_SHUTDOWN.load(Ordering::Relaxed) {
                break;
            }

            std::thread::sleep(POLL_INTERVAL);

            // Poll for messages
            match broker::poll_messages(&agent_name) {
                Ok(messages) => {
                    for msg in messages {
                        deliver_to_pty(&pty_fd, &input_state, &msg.body);
                    }
                }
                Err(_) => {} // SQLite busy or locked, retry next poll
            }

            // Periodic cleanup
            poll_count += 1;
            if poll_count >= CLEANUP_INTERVAL_POLLS {
                poll_count = 0;
                let _ = broker::cleanup_old_messages(MAX_MESSAGE_AGE_SECS);
            }

            // Periodic nudge check for this agent's outbound requests
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

    if let Some(ref pane) = agent.id.pane {
        if let Some(pid_str) = pane.strip_prefix("pty-") {
            if let Ok(pid) = pid_str.parse::<i32>() {
                return unsafe { libc::kill(pid, 0) } == 0;
            }
        }
    }

    // If we can't determine PID, assume alive (could be a relay agent)
    true
}

fn deliver_to_pty(fd: &OwnedFd, input_state: &UserInputState, message: &str) {
    let raw_fd = fd.as_raw_fd();

    // Do not inject while the user is actively typing or has a pending line.
    while !input_state.is_idle() || input_state.has_pending_line() {
        if POLLER_SHUTDOWN.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(INJECT_CHECK_INTERVAL);
    }

    // Write message text
    let _ = write_all_raw(raw_fd, message.as_bytes());
    // Brief pause then send Enter (CR)
    std::thread::sleep(Duration::from_millis(150));
    let _ = write_all_raw(raw_fd, b"\r");
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
