use super::escape_filter::{filter_osc_color_sequences, rewrite_osc_titles};
use super::query_responder::QueryResponder;
use super::replay::ReplayBuffer;
use super::size_owner::{SizeIntent, SizeOwner, SizeOwnership};
use super::waiting::WaitingDetector;
use super::*;

/// Send a viewer everything it needs to render the session as it stands: the
/// terminal modes the agent set, then the retained output.
///
/// The preamble goes first so the replayed bytes land on the same screen buffer
/// and in the same input modes the agent has been drawing with; replaying raw
/// output into a default-mode terminal renders wrong.
fn send_replay(
    tx: &crate::tunnel::TunnelSender,
    input_state: &crate::poller::UserInputState,
    replay: &ReplayBuffer,
) {
    let mut payload = input_state.input_mode().preamble();
    if payload.is_empty() && replay.is_empty() {
        return;
    }
    payload.extend_from_slice(&replay.snapshot());
    tx.send_resync_notice();
    tx.send_resync(payload);
    crate::broker::try_log_event(
        "debug",
        "tunnel",
        &format!("replayed {}B to viewers", replay.len()),
        None,
    );
}

async fn write_sidekar_notice(
    stdout: &mut tokio::io::Stdout,
    tunnel_tx: Option<&crate::tunnel::TunnelSender>,
    message: &str,
    stashed_draft: Option<&str>,
) -> bool {
    let mut out = format!("\r\n\x1b[33m[sidekar]\x1b[0m {message}\r\n");
    if let Some(preview) = stashed_draft {
        out.push_str(&format!(
            "\x1b[33m[sidekar]\x1b[0m Draft saved: \"{preview}\" — ↑ to restore\r\n"
        ));
    }

    crate::tunnel::tunnel_write_async(stdout, tunnel_tx, out.as_bytes()).await
}

fn track_user_input_chunk(
    input_state: &crate::poller::UserInputState,
    line_buf: &mut Vec<u8>,
    chunk: &[u8],
) {
    if chunk.is_empty() {
        return;
    }
    input_state.mark_activity();
    if chunk.contains(&0x1b) {
        return;
    }
    if input_state.take_line_tracking_reset() {
        line_buf.clear();
    }
    for &byte in chunk {
        input_state.discard_stashed_draft();
        if byte == b'\r' || byte == b'\n' {
            line_buf.clear();
            input_state.clear_pending_line();
        } else if byte == 0x7f || byte == 0x08 {
            line_buf.pop();
            input_state.set_pending_line(line_buf);
        } else if byte >= 0x20 {
            line_buf.push(byte);
            input_state.set_pending_line(line_buf);
        }
    }
}

/// True when the chunk commits an answer to whatever the agent asked.
///
/// Arrow keys move a selection but decide nothing; Enter is what resolves the
/// prompt, so that is what releases the injection block.
fn answers_question(chunk: &[u8]) -> bool {
    chunk.iter().any(|b| *b == b'\r' || *b == b'\n')
}

fn pty_ads_enabled() -> bool {
    std::env::var("SIDEKAR_PTY_ADS")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
        .unwrap_or(false)
}

pub(crate) async fn event_loop(
    master: &Arc<OwnedFd>,
    child_pid: libc::pid_t,
    agent_kind: &str,
    tunnel: Option<(crate::tunnel::TunnelSender, crate::tunnel::TunnelReceiver)>,
    nick: &str,
    agent_name: &str,
    input_state: &Arc<crate::poller::UserInputState>,
    mut notice_rx: tokio::sync::mpsc::UnboundedReceiver<crate::poller::PtyNotice>,
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
    let mut pending_recall_esc: Vec<u8> = Vec::with_capacity(8);
    // Split tunnel into sender + receiver (if connected)
    let (tunnel_tx, mut tunnel_rx) = match tunnel {
        Some((tx, rx)) => (Some(tx), Some(rx)),
        None => (None, None),
    };

    // Structured event parser — emits semantic events alongside raw PTY bytes
    let mut event_parser = crate::events::EventParser::new();
    let mut ad_overlay = if pty_ads_enabled() {
        super::ad_overlay::AgentKind::parse(agent_kind)
            .map(|kind| super::ad_overlay::PtyAdOverlay::new(kind, current_terminal_size()))
    } else {
        None
    };
    let mut activity_tick = tokio::time::interval(std::time::Duration::from_secs(30));
    activity_tick.tick().await;

    // Retained output so a viewer attaching mid-session has something to render.
    let mut replay = ReplayBuffer::default();
    // Who currently owns the child PTY's window size.
    let mut size_ownership = SizeOwnership::new();
    // Tracks whether the agent is parked on a question a human must answer.
    let mut waiting = WaitingDetector::new();
    // With no local terminal, sidekar answers the agent's capability probes itself.
    let mut query_responder = (!stdin_is_tty()).then(QueryResponder::new);
    // Set while a viewer is waiting on a full replay after a backlog cut-off.
    let mut pending_resync = false;
    // Last preamble published to the relay, so unchanged modes cost nothing.
    let mut published_preamble: Vec<u8> = Vec::new();

    crate::activity::publish(agent_name, crate::activity::ActivityState::Idle);

    loop {
        tokio::select! {
            biased;

            _ = activity_tick.tick() => {
                input_state.publish_activity(agent_name);
            }

            notice = notice_rx.recv() => {
                let Some(notice) = notice else {
                    continue;
                };
                let delivered = if input_state.sidecar_notice_allowed() {
                    write_sidekar_notice(&mut stdout, tunnel_tx.as_ref(), &notice.message, notice.stashed_draft.as_deref()).await
                } else {
                    false
                };
                let _ = notice.ack.send(delivered);
            }

            // SIGWINCH: resize child PTY
            _ = async {
                match &mut sigwinch {
                    Some(s) => s.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // The physical terminal changed shape, so the local viewer takes
                // the size back from any remote claimant.
                size_ownership.apply(SizeOwner::Local, SizeIntent::Claim);
                let _ = copy_terminal_size(master_fd);
                if let Some(ref mut overlay) = ad_overlay {
                    overlay.resize(current_terminal_size());
                }
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
                        track_user_input_chunk(input_state, &mut line_buf, &filtered);
                        if answers_question(&filtered) {
                            waiting.clear();
                            input_state.set_awaiting_user_input(false);
                        }
                        let _ = write_all_fd_async(&master_async, &filtered).await;
                        input_state.publish_activity(agent_name);
                    }
                    Some(crate::tunnel::TunnelEvent::BusRelay {
                        recipient,
                        sender,
                        body,
                        envelope,
                    }) => {
                        if recipient == agent_name {
                            if let Some(ref envelope) = envelope {
                                match envelope.kind {
                                    crate::message::MessageKind::Request
                                    | crate::message::MessageKind::Handoff => {
                                        if envelope.requires_reply() {
                                            let _ = crate::broker::set_pending(envelope);
                                        } else {
                                            let _ = crate::broker::dismiss_terminal_ack_request(
                                                &envelope.id,
                                            );
                                        }
                                    }
                                    crate::message::MessageKind::Response => {
                                        if let Some(reply_to) = envelope.reply_to.as_deref() {
                                            let _ =
                                                crate::broker::record_reply(reply_to, envelope);
                                        }
                                    }
                                    crate::message::MessageKind::Fyi => {}
                                }
                            }
                            let _ = crate::broker::enqueue_bus_message(
                                &recipient,
                                &sender,
                                &body,
                                true,
                                envelope.as_ref(),
                            );
                        }
                    }
                    Some(crate::tunnel::TunnelEvent::BusPlain(body)) => {
                        let _ = write_all_fd_async(&master_async, body.as_bytes()).await;
                        let _ = write_all_fd_async(&master_async, b"\r\n").await;
                    }
                    Some(crate::tunnel::TunnelEvent::Resize { cols, rows, intent }) => {
                        let intent = SizeIntent::parse(Some(intent.as_str()));
                        if !size_ownership.apply(SizeOwner::Remote, intent) {
                            crate::broker::try_log_event(
                                "debug",
                                "tunnel",
                                &format!(
                                    "ignored viewer resize {cols}x{rows}: size owned by {:?}",
                                    size_ownership.owner()
                                ),
                                None,
                            );
                        } else if set_terminal_size(master_fd, cols, rows).is_ok() {
                            unsafe { libc::kill(child_pid, libc::SIGWINCH) };
                            if let Some(ref mut overlay) = ad_overlay {
                                overlay.resize(Some((cols, rows)));
                            }
                        }
                    }
                    Some(crate::tunnel::TunnelEvent::ReplayRequested) => {
                        if let Some(ref tx) = tunnel_tx {
                            send_replay(tx, input_state, &replay);
                        }
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

                        match crate::poller::UserInputState::draft_recall_from_input(
                            chunk,
                            &mut pending_recall_esc,
                            input_state,
                        ) {
                            crate::poller::DraftRecallInput::Pending => continue,
                            crate::poller::DraftRecallInput::Restore => {
                                input_state.mark_activity();
                                if let Some(draft) = input_state.take_stashed_draft() {
                                    line_buf.clear();
                                    line_buf.extend_from_slice(&draft);
                                    input_state.set_pending_line(&line_buf);
                                    let _ = write_all_fd_async(&master_async, &draft).await;
                                    crate::tunnel::tunnel_println(
                                        "\x1b[33m[sidekar]\x1b[0m Draft restored.",
                                    );
                                }
                                continue;
                            }
                            crate::poller::DraftRecallInput::Forward(bytes) => {
                                input_state.mark_activity();
                                let _ = write_all_fd_async(&master_async, &bytes).await;
                                continue;
                            }
                            crate::poller::DraftRecallInput::NotApplicable => {}
                        }

                        // For local PTY sessions, pass terminal control replies through unchanged.
                        // Codex probes the terminal on startup and expects the real terminal's
                        // responses back on stdin. Swallowing those breaks its renderer.
                        // Don't mark as user activity — these are terminal auto-replies,
                        // not real user input.
                        if chunk.contains(&0x1b) {
                            let _ = write_all_fd_async(&master_async, chunk).await;
                            continue;
                        }

                        track_user_input_chunk(input_state, &mut line_buf, chunk);
                        // Enter commits the answer to whatever the agent asked.
                        if answers_question(chunk) {
                            waiting.clear();
                            input_state.set_awaiting_user_input(false);
                        }
                        let _ = write_all_fd_async(&master_async, chunk).await;
                        input_state.publish_activity(agent_name);
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
                                input_state.mark_pty_output_bytes(raw);

                                // Detached session: no terminal exists to answer the
                                // agent's capability probes, so sidekar answers them.
                                if let Some(ref mut responder) = query_responder
                                    && let Some(reply) = responder.feed(raw)
                                {
                                    let _ = write_all_fd_async(&master_async, &reply).await;
                                }

                                // A full repaint invalidates the tracked screen tail.
                                if super::waiting::resets_screen(raw) {
                                    waiting.clear();
                                }
                                waiting.feed_text(&crate::events::strip_ansi(raw));
                                input_state
                                    .set_awaiting_user_input(waiting.is_question_on_screen());

                                let parsed_events = event_parser.feed(raw);
                                for event in &parsed_events {
                                    if matches!(event, crate::events::AgentEvent::Status { .. }) {
                                        input_state.mark_spinner_activity();
                                    }
                                }
                                input_state.publish_activity(agent_name);
                                // Preserve terminal transparency except for OSC window-title
                                // sequences, where we prefix the agent nickname.
                                let local_data = if nick_prefix.is_empty() {
                                    std::borrow::Cow::Borrowed(raw)
                                } else {
                                    rewrite_osc_titles(raw, &nick_prefix)
                                };
                                let overlay_data = ad_overlay
                                    .as_mut()
                                    .and_then(|overlay| overlay.feed(&local_data));
                                match overlay_data.as_ref() {
                                    Some(patch) => {
                                        let mut out = Vec::with_capacity(local_data.len() + patch.len());
                                        out.extend_from_slice(&local_data);
                                        out.extend_from_slice(patch);
                                        if stdout.write_all(&out).await.is_err() {
                                            break;
                                        }
                                    }
                                    None => {
                                        if stdout.write_all(&local_data).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                                let _ = stdout.flush().await;

                                // Fan-out to tunnel with normalized control sequences for the web terminal.
                                if let Some(ref tx) = tunnel_tx {
                                    let filtered = filter_osc_color_sequences(raw);
                                    let mut tunnel_data = if nick_prefix.is_empty() {
                                        filtered.into_owned()
                                    } else {
                                        rewrite_osc_titles(&filtered, &nick_prefix).into_owned()
                                    };
                                    if let Some(data) = overlay_data.as_ref() {
                                        tunnel_data.extend_from_slice(data);
                                    }
                                    replay.push(&tunnel_data);
                                    tx.send_data(tunnel_data);

                                    // Modes change rarely; only publish on a diff.
                                    let preamble = input_state.input_mode().preamble();
                                    if preamble != published_preamble {
                                        tx.send_input_mode(&preamble);
                                        published_preamble = preamble;
                                    }

                                    // Emit structured events alongside raw bytes
                                    for event in parsed_events {
                                        tx.send_event(crate::events::event_to_json(&event));
                                    }

                                    // A viewer that fell too far behind was cut off
                                    // rather than fed a stream with holes in it.
                                    // Replay it whole once the socket drains.
                                    if tx.take_overflow() {
                                        pending_resync = true;
                                        crate::broker::try_log_event(
                                            "debug",
                                            "tunnel",
                                            "viewer backlog exceeded; queued resync",
                                            None,
                                        );
                                    }
                                    if pending_resync && tx.is_drained() {
                                        pending_resync = false;
                                        send_replay(tx, input_state, &replay);
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

    wait_child_exit_or_terminate(child_pid)
}

#[cfg(test)]
mod tests;
