//! Bus message poller — reads from the SQLite bus_queue and delivers
//! messages to the local agent via tmux paste or PTY write.

use crate::broker;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

static POLLER_SHUTDOWN: AtomicBool = AtomicBool::new(false);

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const CLEANUP_INTERVAL_POLLS: u32 = 120; // clean old messages every 60s (120 * 500ms)
const MAX_MESSAGE_AGE_SECS: u64 = 3600;

/// How the poller delivers messages to the local agent.
pub enum DeliverySink {
    /// Write to a PTY master fd (for `sidekar <cmd>` wrapper).
    PtyFd(Arc<OwnedFd>),
    /// Paste into a tmux pane (for MCP server in tmux).
    TmuxPane(String),
}

/// Signal the poller to stop.
pub fn shutdown_poller() {
    POLLER_SHUTDOWN.store(true, Ordering::Relaxed);
}

/// Start the background poller thread. Returns immediately.
pub fn start_poller(agent_name: String, sink: DeliverySink) {
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
                        deliver(&sink, &msg.body);
                    }
                }
                Err(_) => {} // SQLite busy or locked, retry next poll
            }

            // Periodic cleanup of old undelivered messages
            poll_count += 1;
            if poll_count >= CLEANUP_INTERVAL_POLLS {
                poll_count = 0;
                let _ = broker::cleanup_old_messages(MAX_MESSAGE_AGE_SECS);
            }
        }
    });
}

fn deliver(sink: &DeliverySink, message: &str) {
    match sink {
        DeliverySink::PtyFd(fd) => {
            let raw_fd = fd.as_raw_fd();
            // Write message text
            let _ = write_all_raw(raw_fd, message.as_bytes());
            // Brief pause then send Enter (CR)
            std::thread::sleep(Duration::from_millis(150));
            let _ = write_all_raw(raw_fd, b"\r");
        }
        DeliverySink::TmuxPane(pane) => {
            let _ = crate::ipc::send_to_pane(pane, message);
        }
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
