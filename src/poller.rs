//! Bus message poller — reads from the SQLite bus_queue and delivers
//! messages to the local agent via PTY write.

use crate::broker;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

static POLLER_SHUTDOWN: AtomicBool = AtomicBool::new(false);

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const CLEANUP_INTERVAL_POLLS: u32 = 120; // clean old messages every 60s (120 * 500ms)
const MAX_MESSAGE_AGE_SECS: u64 = 3600;

/// Signal the poller to stop.
pub fn shutdown_poller() {
    POLLER_SHUTDOWN.store(true, Ordering::Relaxed);
}

/// Start the background poller thread. Returns immediately.
pub fn start_poller(agent_name: String, pty_fd: Arc<OwnedFd>) {
    POLLER_SHUTDOWN.store(false, Ordering::Relaxed);

    std::thread::spawn(move || {
        let mut poll_count: u32 = 0;

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
                sweep_dead_agents();
            }
        }
    });
}

/// Sweep dead agents from the broker. Checks each agent's PTY PID and
/// unregisters any whose process is no longer alive.
fn sweep_dead_agents() {
    let agents = match broker::list_agents(None) {
        Ok(a) => a,
        Err(_) => return,
    };
    for agent in agents {
        if let Some(ref pane) = agent.id.pane {
            if let Some(pid_str) = pane.strip_prefix("pty-") {
                if let Ok(pid) = pid_str.parse::<i32>() {
                    if unsafe { libc::kill(pid, 0) } != 0 {
                        let _ = broker::unregister_agent(&agent.id.name);
                    }
                }
            }
        }
    }
}

fn deliver_to_pty(fd: &OwnedFd, message: &str) {
    let raw_fd = fd.as_raw_fd();
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
