use super::*;

/// Start the relay tunnel. Returns `(TunnelSender, pipe_fd)` on success.
pub(super) async fn start_relay(
    bus_name: &str,
    cwd: &str,
    nick: &str,
) -> (Option<crate::tunnel::TunnelSender>, Option<i32>) {
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
    let pipe_fd = bridge_tunnel_input(rx, bus_name);
    (Some(tx), pipe_fd)
}

/// Stop the relay tunnel, clear the global output tunnel.
pub(super) fn stop_relay(tx: Option<crate::tunnel::TunnelSender>) {
    if let Some(tx) = tx {
        tx.shutdown();
    }
    crate::tunnel::clear_output_tunnel();
}

/// Spawn a task that drains `TunnelReceiver` into a pipe fd for the poll loop.
fn bridge_tunnel_input(mut rx: crate::tunnel::TunnelReceiver, bus_name: &str) -> Option<i32> {
    use std::os::unix::io::FromRawFd;
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return None;
    }
    let read_fd = fds[0];
    let write_fd = fds[1];
    unsafe { libc::fcntl(write_fd, libc::F_SETFL, libc::O_NONBLOCK) };
    let bus = bus_name.to_string();
    tokio::spawn(async move {
        use std::io::Write as _;
        let mut pipe = unsafe { std::fs::File::from_raw_fd(write_fd) };
        while let Some(event) = rx.recv().await {
            match event {
                crate::tunnel::TunnelEvent::Data(data) => {
                    let _ = pipe.write_all(&data);
                }
                crate::tunnel::TunnelEvent::BusRelay {
                    recipient,
                    sender,
                    body,
                    envelope,
                } => {
                    if let Some(envelope) = envelope {
                        match envelope.kind {
                            crate::message::MessageKind::Request
                            | crate::message::MessageKind::Handoff => {
                                let _ = broker::set_pending(&envelope);
                            }
                            crate::message::MessageKind::Response => {
                                if let Some(reply_to) = envelope.reply_to.as_deref() {
                                    let _ = broker::record_reply(reply_to, &envelope);
                                }
                            }
                            crate::message::MessageKind::Fyi => {}
                        }
                    }
                    let _ = broker::enqueue_message(&sender, &recipient, &body);
                }
                crate::tunnel::TunnelEvent::BusPlain(text) => {
                    let _ = broker::enqueue_message("relay", &bus, &text);
                }
                crate::tunnel::TunnelEvent::Disconnected => {}
            }
        }
        drop(pipe);
    });
    Some(read_fd)
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
        broker::try_log_event(
            "debug",
            "bus",
            "received",
            Some(&format!("from={}", msg.sender)),
        );
        let steering = ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text }],
        };
        let _ = session::append_message(session_id, &steering);
        history.push(steering);
    }
    n
}
