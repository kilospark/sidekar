use super::*;
use crate::tunnel::tunnel_println;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// SGR: italic + bright cyan foreground + dark gray background (inbound bus activity).
const BUS_CONSOLE_LINE: &str = "\x1b[48;5;237m\x1b[3m\x1b[96m";
const BUS_CONSOLE_RESET: &str = "\x1b[0m";

pub(super) struct TunnelInputBridge {
    read_fd: i32,
    paused: Arc<AtomicBool>,
    closed: bool,
}

impl TunnelInputBridge {
    pub(super) fn fd(&self) -> Option<i32> {
        (!self.closed).then_some(self.read_fd)
    }

    pub(super) fn pause(&self) {
        self.paused.store(true, Ordering::Relaxed);
    }

    pub(super) fn resume(&self) {
        self.paused.store(false, Ordering::Relaxed);
    }

    pub(super) fn drain(&self) {
        if !self.closed {
            drain_pipe_fd(self.read_fd);
        }
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        unsafe {
            libc::close(self.read_fd);
        }
    }
}

impl Drop for TunnelInputBridge {
    fn drop(&mut self) {
        self.close();
    }
}

/// Start the relay tunnel. Returns `(TunnelSender, input bridge)` on success.
pub(super) async fn start_relay(
    bus_name: &str,
    cwd: &str,
    nick: &str,
) -> (
    Option<crate::tunnel::TunnelSender>,
    Option<TunnelInputBridge>,
) {
    let token = match crate::auth::auth_token() {
        Some(t) => t,
        None => {
            broker::try_log_error(
                "relay",
                "skipped: no device token; run: sidekar device login",
                None,
            );
            return (None, None);
        }
    };
    broker::try_log_event("debug", "relay", "connecting", None);
    let (cols, rows) = terminal_size().unwrap_or((80, 24));
    let (tx, rx) =
        match crate::tunnel::connect(&token, bus_name, "sidekar-repl", cwd, nick, cols, rows).await
        {
            Ok(pair) => pair,
            Err(e) => {
                broker::try_log_error("relay", &format!("{e:#}"), None);
                return (None, None);
            }
        };
    broker::try_log_event("debug", "relay", "connected", None);
    crate::tunnel::set_output_tunnel(tx.clone());

    // Bridge tunnel input (web terminal keystrokes) into a pipe fd so the
    // synchronous poll loop in read_input_or_bus can multiplex it with stdin.
    let bridge = bridge_tunnel_input(rx, bus_name);
    (Some(tx), bridge)
}

/// Stop the relay tunnel, drop the input bridge, clear the global output tunnel.
pub(super) fn stop_relay(
    tx: Option<crate::tunnel::TunnelSender>,
    bridge: Option<TunnelInputBridge>,
) {
    if let Some(tx) = tx {
        tx.shutdown();
    }
    drop(bridge);
    crate::tunnel::clear_output_tunnel();
}

/// Attach this REPL's stdin/stdout to another machine's relay tunnel (web-terminal protocol).
pub(super) async fn run_remote_relay_attach(session_id: &str) {
    let Some(token) = crate::auth::auth_token() else {
        tunnel_println("\x1b[31mNot logged in. Run: sidekar device login\x1b[0m");
        return;
    };
    if let Err(e) = crate::tunnel::attach_remote_relay_terminal(&token, session_id).await {
        tunnel_println(&format!("\x1b[31m{e:#}\x1b[0m"));
    }
}

fn drain_pipe_fd(fd: i32) {
    let mut buf = [0u8; 256];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n > 0 {
            continue;
        }
        if n == 0 {
            break;
        }
        let err = std::io::Error::last_os_error();
        if matches!(
            err.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
        ) {
            break;
        }
        break;
    }
}

/// Spawn a task that drains `TunnelReceiver` into a pipe fd for the poll loop.
fn bridge_tunnel_input(
    mut rx: crate::tunnel::TunnelReceiver,
    bus_name: &str,
) -> Option<TunnelInputBridge> {
    use std::os::unix::io::FromRawFd;
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return None;
    }
    let read_fd = fds[0];
    let write_fd = fds[1];
    unsafe { libc::fcntl(read_fd, libc::F_SETFL, libc::O_NONBLOCK) };
    unsafe { libc::fcntl(write_fd, libc::F_SETFL, libc::O_NONBLOCK) };
    let paused = Arc::new(AtomicBool::new(false));
    let paused_task = paused.clone();
    let bus = bus_name.to_string();
    tokio::spawn(async move {
        use std::io::Write as _;
        let mut pipe = unsafe { std::fs::File::from_raw_fd(write_fd) };
        while let Some(event) = rx.recv().await {
            match event {
                crate::tunnel::TunnelEvent::Data(data) => {
                    if !paused_task.load(Ordering::Relaxed) {
                        let _ = pipe.write_all(&data);
                    }
                }
                crate::tunnel::TunnelEvent::BusRelay {
                    recipient,
                    sender,
                    body,
                    envelope,
                } => {
                    if let Some(ref envelope) = envelope {
                        match envelope.kind {
                            crate::message::MessageKind::Request
                            | crate::message::MessageKind::Handoff => {
                                if envelope.requires_reply() {
                                    let _ = broker::set_pending(envelope);
                                } else {
                                    let _ = broker::dismiss_terminal_ack_request(&envelope.id);
                                }
                            }
                            crate::message::MessageKind::Response => {
                                if let Some(reply_to) = envelope.reply_to.as_deref() {
                                    let _ = broker::record_reply(reply_to, envelope);
                                }
                            }
                            crate::message::MessageKind::Fyi => {}
                        }
                    }
                    let _ = broker::enqueue_bus_message(
                        &recipient,
                        &sender,
                        &body,
                        true,
                        envelope.as_ref(),
                    );
                }
                crate::tunnel::TunnelEvent::BusPlain(text) => {
                    let _ = broker::enqueue_message("relay", &bus, &text);
                }
                // The REPL owns its own rendering and sizing, so viewer resize
                // and replay requests are not actionable here.
                crate::tunnel::TunnelEvent::Resize { .. }
                | crate::tunnel::TunnelEvent::ReplayRequested
                | crate::tunnel::TunnelEvent::Disconnected => {}
            }
        }
        drop(pipe);
    });
    Some(TunnelInputBridge {
        read_fd,
        paused,
        closed: false,
    })
}

pub(super) fn terminal_size() -> Option<(u16, u16)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ, &mut ws) } != 0 {
        return None;
    }
    if ws.ws_col == 0 || ws.ws_row == 0 {
        return None;
    }
    Some((ws.ws_col, ws.ws_row))
}

pub(super) fn inject_bus_messages(
    bus_name: &str,
    history: &mut Vec<ChatMessage>,
    session_id: &str,
) -> usize {
    let Ok(messages) = broker::poll_messages(bus_name) else {
        return 0;
    };
    let n = messages.len();
    for msg in messages {
        let text = format!("[Bus message from {}]: {}", msg.sender, msg.body);
        let inbox_detail = serde_json::json!({
            "sender": msg.sender,
            "body": msg.body,
            "recipient": msg.recipient,
        })
        .to_string();
        broker::try_log_event("info", "inbox", "received", Some(&inbox_detail));
        let display = format!(
            "{}[bus] {} says: {}{}",
            BUS_CONSOLE_LINE, msg.sender, msg.body, BUS_CONSOLE_RESET,
        );
        tunnel_println(&display);
        let steering = ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text }],
        };
        let _ = session::append_message(session_id, &steering);
        history.push(steering);
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::io::FromRawFd;

    #[test]
    fn drain_pipe_fd_clears_buffered_bytes() {
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let read_fd = fds[0];
        let write_fd = fds[1];
        unsafe {
            libc::fcntl(read_fd, libc::F_SETFL, libc::O_NONBLOCK);
        }

        let mut writer = unsafe { std::fs::File::from_raw_fd(write_fd) };
        use std::io::Write as _;
        writer.write_all(b"stale-input").expect("write pipe");

        drain_pipe_fd(read_fd);

        let mut reader = unsafe { std::fs::File::from_raw_fd(read_fd) };
        let mut buf = [0u8; 16];
        let err = reader.read(&mut buf).expect_err("pipe should be empty");
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    fn tunnel_input_bridge_close_disables_fd() {
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        unsafe {
            libc::close(fds[1]);
        }
        let mut bridge = TunnelInputBridge {
            read_fd: fds[0],
            paused: Arc::new(AtomicBool::new(false)),
            closed: false,
        };
        assert_eq!(bridge.fd(), Some(fds[0]));
        bridge.close();
        assert_eq!(bridge.fd(), None);
    }
}
