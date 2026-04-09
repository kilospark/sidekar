use super::*;
use super::escape_filter::{filter_osc_color_sequences, rewrite_osc_titles};

pub(crate) async fn event_loop(
    master: &Arc<OwnedFd>,
    child_pid: libc::pid_t,
    tunnel: Option<(crate::tunnel::TunnelSender, crate::tunnel::TunnelReceiver)>,
    nick: &str,
    agent_name: &str,
    input_state: &Arc<crate::poller::UserInputState>,
) -> i32 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::signal::unix::{SignalKind, signal};

    let nick_prefix = if nick.is_empty() {
        String::new()
    } else {
        format!("{nick} - ")
    };

    let master_fd = master.as_raw_fd();

    // Wrap master fd for async I/O
    let master_async = match tokio::io::unix::AsyncFd::new(master_fd) {
        Ok(fd) => fd,
        Err(_e) => {
            // silent — error code returned
            return 1;
        }
    };

    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    // Signal registration can fail (FD limits, sandbox). Do not panic — abort would kill the PTY wrapper.
    let mut sigwinch = match signal(SignalKind::window_change()) {
        Ok(s) => Some(s),
        Err(e) => {
            crate::broker::try_log_error(
                "signal",
                &format!("SIGWINCH handler unavailable: {e}"),
                None,
            );
            None
        }
    };
    let mut sigterm_sig = match signal(SignalKind::terminate()) {
        Ok(s) => Some(s),
        Err(e) => {
            crate::broker::try_log_error(
                "signal",
                &format!("SIGTERM handler unavailable: {e}"),
                None,
            );
            None
        }
    };

    let mut buf_in = [0u8; 4096];
    let mut buf_out = [0u8; 8192];

    // Line buffer for pending-user-input tracking.
    let mut line_buf: Vec<u8> = Vec::with_capacity(256);
    // Split tunnel into sender + receiver (if connected)
    let (tunnel_tx, mut tunnel_rx) = match tunnel {
        Some((tx, rx)) => (Some(tx), Some(rx)),
        None => (None, None),
    };

    // Structured event parser — emits semantic events alongside raw PTY bytes
    let mut event_parser = crate::events::EventParser::new();

    loop {
        tokio::select! {
            biased;

            // SIGWINCH: resize child PTY
            _ = async {
                match &mut sigwinch {
                    Some(s) => s.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                let _ = copy_terminal_size(master_fd);
                if let (Some(tx), Some((cols, rows))) = (tunnel_tx.as_ref(), current_terminal_size()) {
                    tx.send_terminal_resize(cols, rows);
                }
            }

            // SIGTERM: forward to child, exit
            _ = async {
                match &mut sigterm_sig {
                    Some(s) => s.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                unsafe { libc::kill(child_pid, libc::SIGTERM) };
                break;
            }

            // Tunnel → master fd (browser input injected into agent)
            event = async {
                match tunnel_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match event {
                    Some(crate::tunnel::TunnelEvent::Data(data)) => {
                        // Filter out OSC color queries from browser's xterm.js
                        let filtered = filter_osc_color_sequences(&data);
                        let _ = write_all_fd(master_fd, &filtered);
                    }
                    Some(crate::tunnel::TunnelEvent::BusRelay {
                        recipient,
                        sender,
                        body,
                        envelope,
                    }) => {
                        if recipient == agent_name {
                            if let Some(envelope) = envelope {
                                match envelope.kind {
                                    crate::message::MessageKind::Request
                                    | crate::message::MessageKind::Handoff => {
                                        let _ = crate::broker::set_pending(&envelope);
                                    }
                                    crate::message::MessageKind::Response => {
                                        if let Some(reply_to) = envelope.reply_to.as_deref() {
                                            let _ = crate::broker::record_reply(reply_to, &envelope);
                                        }
                                    }
                                    crate::message::MessageKind::Fyi => {}
                                }
                            }
                            let _ = crate::broker::enqueue_message(&sender, &recipient, &body);
                        }
                    }
                    Some(crate::tunnel::TunnelEvent::BusPlain(body)) => {
                        let _ = write_all_fd(master_fd, body.as_bytes());
                        let _ = write_all_fd(master_fd, b"\r\n");
                    }
                    Some(crate::tunnel::TunnelEvent::Disconnected) => {}
                    None => {
                        tunnel_rx = None;
                    }
                }
            }

            // stdin → master fd (user typing forwarded to agent)
            result = stdin.read(&mut buf_in) => {
                match result {
                    Ok(0) | Err(_) => break, // stdin closed
                    Ok(n) => {
                        let chunk = &buf_in[..n];

                        // For local PTY sessions, pass terminal control replies through unchanged.
                        // Codex probes the terminal on startup and expects the real terminal's
                        // responses back on stdin. Swallowing those breaks its renderer.
                        // Don't mark as user activity — these are terminal auto-replies,
                        // not real user input.
                        if chunk.contains(&0x1b) {
                            let _ = write_all_fd(master_fd, chunk);
                            continue;
                        }

                        input_state.mark_activity();

                        for &byte in chunk {
                            if byte == b'\r' || byte == b'\n' {
                                line_buf.clear();
                                input_state.clear_pending_line();
                                let _ = write_all_fd(master_fd, &[byte]);
                            } else if byte == 0x7f || byte == 0x08 {
                                line_buf.pop();
                                input_state.set_pending_line(&line_buf);
                                let _ = write_all_fd(master_fd, &[byte]);
                            } else {
                                line_buf.push(byte);
                                input_state.set_pending_line(&line_buf);
                                let _ = write_all_fd(master_fd, &[byte]);
                            }
                        }
                    }
                }
            }

            // master fd → stdout AND tunnel (agent output)
            result = master_async.readable() => {
                match result {
                    Ok(mut guard) => {
                        match guard.try_io(|_| {
                            let n = unsafe {
                                libc::read(master_fd, buf_out.as_mut_ptr() as *mut libc::c_void, buf_out.len())
                            };
                            if n > 0 {
                                Ok(n as usize)
                            } else if n == 0 {
                                Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "child exited"))
                            } else {
                                Err(std::io::Error::last_os_error())
                            }
                        }) {
                            Ok(Ok(n)) => {
                                let raw = &buf_out[..n];
                                // Preserve terminal transparency except for OSC window-title
                                // sequences, where we prefix the agent nickname.
                                let local_data = if nick_prefix.is_empty() {
                                    std::borrow::Cow::Borrowed(raw)
                                } else {
                                    rewrite_osc_titles(raw, &nick_prefix)
                                };
                                if stdout.write_all(&local_data).await.is_err() {
                                    break;
                                }
                                let _ = stdout.flush().await;

                                // Fan-out to tunnel with normalized control sequences for the web terminal.
                                if let Some(ref tx) = tunnel_tx {
                                    let filtered = filter_osc_color_sequences(raw);
                                    let tunnel_data = if nick_prefix.is_empty() {
                                        filtered.into_owned()
                                    } else {
                                        rewrite_osc_titles(&filtered, &nick_prefix).into_owned()
                                    };
                                    tx.send_data(tunnel_data);

                                    // Emit structured events alongside raw bytes
                                    for event in event_parser.feed(raw) {
                                        tx.send_event(crate::events::event_to_json(&event));
                                    }
                                }
                            }
                            Ok(Err(_)) => break,
                            Err(_would_block) => continue,
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    // Flush the async stdout — process::exit() won't run Drop impls, and
    // the tokio stdout has its own buffer separate from std::io::stdout().
    // The child's final escape sequences (rmcup etc.) must be flushed now.
    let _ = stdout.flush().await;

    // Flush any pending events before shutting down
    if let Some(ref tx) = tunnel_tx {
        for event in event_parser.flush() {
            tx.send_event(crate::events::event_to_json(&event));
        }
    }

    // Shut down tunnel gracefully
    if let Some(tx) = tunnel_tx {
        tx.shutdown();
    }

    // Wait for child to exit
    let mut status: libc::c_int = 0;
    unsafe { libc::waitpid(child_pid, &mut status, 0) };

    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        1
    }
}
