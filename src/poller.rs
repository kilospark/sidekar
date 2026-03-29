//! Bus message poller — reads from the SQLite bus_queue and delivers
//! messages to the local agent via PTY write.

use crate::broker;
use crate::transport::{Broker as BrokerTransport, RelayHttp, Transport};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

static POLLER_SHUTDOWN: AtomicBool = AtomicBool::new(false);

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const CLEANUP_INTERVAL_POLLS: u32 = 120; // clean old messages every 60s (120 * 500ms)
const NUDGE_INTERVAL_POLLS: u32 = 120;   // check nudges every 60s
const MAX_MESSAGE_AGE_SECS: u64 = 3600;
const NUDGE_INTERVAL_SECS: u64 = 60;
const NUDGE_BACKOFF_SECS: u64 = 120;
const NUDGE_MAX: u32 = 5;

/// Signal the poller to stop.
pub fn shutdown_poller() {
    POLLER_SHUTDOWN.store(true, Ordering::Relaxed);
}

/// Start the background poller thread. Returns immediately.
pub fn start_poller(agent_name: String, pty_fd: Arc<OwnedFd>) {
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
                        deliver_to_pty(&pty_fd, &msg.body);
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
        // Calculate required wait time based on nudge count
        let wait_secs = if request.nudge_count == 0 {
            NUDGE_INTERVAL_SECS
        } else {
            NUDGE_BACKOFF_SECS
        };

        // Skip if not enough time has passed
        let elapsed = now.saturating_sub(request.created_at as u64);
        let last_nudge_elapsed = if request.nudge_count == 0 {
            elapsed
        } else {
            // Approximate: assume nudges were sent at regular intervals
            elapsed.saturating_sub(
                NUDGE_INTERVAL_SECS + (request.nudge_count.saturating_sub(1) as u64 * NUDGE_BACKOFF_SECS)
            )
        };

        if last_nudge_elapsed < wait_secs {
            continue;
        }

        // Check if we've hit max nudges
        if request.nudge_count >= NUDGE_MAX {
            continue;
        }

        // Check if the pending message still exists (hasn't been answered)
        if broker::pending_message(&request.msg_id).ok().flatten().is_none() {
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
            "[sidekar] You have an unanswered request from {}. Reply using bus_send or bus_done with reply_to: \"{}\"",
            request.sender_label, request.msg_id
        );

        let delivery_result = match request.transport_name.as_str() {
            "broker" => BrokerTransport.deliver(&request.transport_target, &nudge_msg, "sidekar"),
            "relay_http" => RelayHttp.deliver(&request.transport_target, &nudge_msg, "sidekar"),
            _ => continue,
        };

        if delivery_result.is_ok() {
            let _ = broker::increment_nudge_count(&request.msg_id);
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

fn deliver_to_pty(fd: &OwnedFd, message: &str) {
    let raw_fd = fd.as_raw_fd();

    // Wait for user to stop typing (1 second of inactivity)
    // Check multiple times - if there's input, wait more
    let mut quiet_count = 0;
    while quiet_count < 10 {
        if is_data_available(raw_fd) {
            quiet_count = 0; // user typed something, reset counter
        } else {
            quiet_count += 1;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Save any user input that was being typed
    let saved_input = read_pending_input(raw_fd);

    // Write message text
    let _ = write_all_raw(raw_fd, message.as_bytes());
    // Brief pause then send Enter (CR)
    std::thread::sleep(Duration::from_millis(150));
    let _ = write_all_raw(raw_fd, b"\r");

    // Restore user's saved input
    if !saved_input.is_empty() {
        std::thread::sleep(Duration::from_millis(100));
        let _ = write_all_raw(raw_fd, saved_input.as_bytes());
    }
}

/// Check if there's data available to read from the PTY
fn is_data_available(fd: i32) -> bool {
    use std::mem::MaybeUninit;

    let mut pollfd = MaybeUninit::<libc::pollfd>::zeroed();
    unsafe {
        let pfd = pollfd.as_mut_ptr();
        (*pfd).fd = fd;
        (*pfd).events = libc::POLLIN;
        (*pfd).revents = 0;

        if libc::poll(pfd, 1, 0) > 0 {
            ((*pfd).revents & libc::POLLIN) != 0
        } else {
            false
        }
    }
}

/// Read any pending input from the PTY (user's typing)
fn read_pending_input(fd: i32) -> String {
    let mut buf = [0u8; 1024];
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n > 0 {
        String::from_utf8_lossy(&buf[..n as usize]).to_string()
    } else {
        String::new()
    }
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
