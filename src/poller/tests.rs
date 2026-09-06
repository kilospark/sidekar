use super::*;
use std::os::fd::AsRawFd;

#[test]
fn stash_moves_pending_line_and_clears_tracking() {
    let state = UserInputState::new();
    state.set_pending_line(b"fix the bu");
    assert!(state.stash_pending_line_for_inject());
    assert!(!state.has_pending_line());
    assert!(state.has_stashed_draft());
    assert!(state.take_line_tracking_reset());
    assert!(!state.take_line_tracking_reset());
}

#[test]
fn stash_noop_when_pending_empty() {
    let state = UserInputState::new();
    assert!(!state.stash_pending_line_for_inject());
    assert!(!state.has_stashed_draft());
}

#[test]
fn take_stashed_draft_clears_stash() {
    let state = UserInputState::new();
    state.set_pending_line(b"hello");
    state.stash_pending_line_for_inject();
    assert_eq!(
        state.take_stashed_draft().as_deref(),
        Some(b"hello".as_ref())
    );
    assert!(!state.has_stashed_draft());
}

#[test]
fn draft_recall_up_arrow_csi() {
    let state = UserInputState::new();
    state.set_pending_line(b"hello");
    state.stash_pending_line_for_inject();
    let mut pending = Vec::new();
    assert!(matches!(
        UserInputState::draft_recall_from_input(b"\x1b", &mut pending, &state),
        DraftRecallInput::Pending
    ));
    assert!(matches!(
        UserInputState::draft_recall_from_input(b"[A", &mut pending, &state),
        DraftRecallInput::Restore
    ));
}

#[test]
fn draft_recall_other_key_forwards_and_drops_stash() {
    let state = UserInputState::new();
    state.set_pending_line(b"hello");
    state.stash_pending_line_for_inject();
    let mut pending = Vec::new();
    match UserInputState::draft_recall_from_input(b"x", &mut pending, &state) {
        DraftRecallInput::Forward(bytes) => assert_eq!(bytes, b"x"),
        other => panic!("expected Forward, got {other:?}"),
    }
    assert!(!state.has_stashed_draft());
}

#[test]
fn draft_pending_no_longer_blocks_submit_once_typing_stops() {
    // Was: a buffered draft blocked submission outright, keystroke or not.
    // That protected the draft by never pasting over it, and the cost was a
    // pane that went deaf for as long as text sat in its input box, with no
    // timeout and no way past it — not the interrupt path, not the deadline.
    // The draft is now protected by being lifted out and handed back around
    // the paste, so blocking is no longer what keeps it safe.
    let state = UserInputState::new();
    state.set_pending_line(b"half typed");
    assert!(state.is_idle(), "nobody has touched the keyboard");
    assert!(state.has_pending_line());
    assert!(!pty_submit_wait_blocked_for(&state, false));

    // Still deferred while the typing is actually happening.
    state.mark_activity();
    assert!(pty_submit_wait_blocked_for(&state, false));
}

#[test]
fn agent_working_heuristic_tracks_recent_output() {
    let state = UserInputState::new();
    assert!(!state.is_agent_working());
    state.mark_pty_output();
    assert!(state.is_agent_working());
}

#[test]
fn terminal_state_blocks_sidecar_notice_in_alternate_screen() {
    let state = UserInputState::new();
    assert!(state.sidecar_notice_allowed());
    state.mark_pty_output_bytes(b"\x1b[?1049h");
    assert!(!state.sidecar_notice_allowed());
    state.mark_pty_output_bytes(b"\x1b[?1049l");
    assert!(state.sidecar_notice_allowed());
}

#[test]
fn terminal_state_blocks_sidecar_notice_while_cursor_hidden() {
    let state = UserInputState::new();
    assert!(state.sidecar_notice_allowed());
    state.mark_pty_output_bytes(b"\x1b[?25l");
    assert!(!state.sidecar_notice_allowed());
    state.mark_pty_output_bytes(b"\x1b[?25h");
    assert!(state.sidecar_notice_allowed());
}

#[test]
fn terminal_state_tracks_split_escape_sequences() {
    let state = UserInputState::new();
    state.mark_pty_output_bytes(b"\x1b[?");
    assert!(state.sidecar_notice_allowed());
    state.mark_pty_output_bytes(b"1049h");
    assert!(!state.sidecar_notice_allowed());
}

#[test]
fn terminal_notice_visibility_does_not_block_prompt_submission() {
    let state = UserInputState::new();
    state.update_terminal_state(b"\x1b[?1049h");

    assert!(!state.sidecar_notice_allowed());
    assert!(!pty_submit_wait_blocked_for(&state, false));
}

#[test]
fn interrupt_submit_does_not_wait_for_agent_output_busy() {
    let state = UserInputState::new();
    state.mark_pty_output();

    assert!(pty_submit_wait_blocked_for(&state, false));
    assert!(!pty_submit_wait_blocked_for(&state, true));
}

#[test]
fn verified_tui_agents_use_bracketed_paste_submit_encoding() {
    assert_eq!(
        submit_encoding_for_agent("claude"),
        PtySubmitEncoding::BracketedPaste
    );
    assert_eq!(
        submit_encoding_for_agent("codex"),
        PtySubmitEncoding::BracketedPaste
    );
    assert_eq!(
        submit_encoding_for_agent("copilot"),
        PtySubmitEncoding::BracketedPaste
    );
    assert_eq!(
        submit_encoding_for_agent("cursor-agent"),
        PtySubmitEncoding::BracketedPaste
    );
    assert_eq!(
        submit_encoding_for_agent("agent"),
        PtySubmitEncoding::BracketedPaste
    );
    assert_eq!(
        submit_encoding_for_agent("cursor"),
        PtySubmitEncoding::BracketedPaste
    );
    assert_eq!(
        submit_encoding_for_agent("grok"),
        PtySubmitEncoding::BracketedPaste
    );
    assert_eq!(
        submit_encoding_for_agent("opencode"),
        PtySubmitEncoding::BracketedPaste
    );
    assert_eq!(
        submit_encoding_for_agent("gemini"),
        PtySubmitEncoding::BracketedPaste
    );
    assert_eq!(
        submit_encoding_for_agent("pi"),
        PtySubmitEncoding::BracketedPaste
    );
}

#[test]
fn bracketed_paste_submit_wraps_message_without_enter() {
    let bytes = encode_submit_input("line one\nline two", PtySubmitEncoding::BracketedPaste);
    assert_eq!(bytes, b"\x1b[200~line one\nline two\x1b[201~");
}

#[test]
fn raw_submit_keeps_message_bytes_unchanged() {
    let bytes = encode_submit_input("line one\nline two", PtySubmitEncoding::Raw);
    assert_eq!(bytes, b"line one\nline two");
}

#[test]
fn interrupt_sequence_only_for_tui_submit_agents() {
    assert_eq!(
        interrupt_sequence_for_agent("codex"),
        Some(b"\x1b".as_ref())
    );
    assert_eq!(
        interrupt_sequence_for_agent("cursor-agent"),
        Some(b"\x1b".as_ref())
    );
    assert_eq!(interrupt_sequence_for_agent("unknown"), None);
}

#[test]
fn format_draft_preview_truncates_long_lines() {
    let long = "a".repeat(100);
    let preview = format_draft_preview(long.as_bytes());
    assert!(preview.ends_with('…'));
    assert!(preview.chars().count() <= 80);
}

#[test]
fn observed_bracketed_paste_overrides_agent_default() {
    let state = UserInputState::new();
    state.mark_pty_output_bytes(b"\x1b[?2004h");
    assert_eq!(
        resolve_submit_encoding(&state, PtySubmitEncoding::Raw),
        PtySubmitEncoding::BracketedPaste
    );
}

#[test]
fn observed_raw_mode_overrides_agent_default() {
    let state = UserInputState::new();
    state.mark_pty_output_bytes(b"\x1b[?2004h");
    state.mark_pty_output_bytes(b"\x1b[?2004l");
    assert_eq!(
        resolve_submit_encoding(&state, PtySubmitEncoding::BracketedPaste),
        PtySubmitEncoding::Raw
    );
}

#[test]
fn unobserved_paste_mode_falls_back_to_agent_default() {
    let state = UserInputState::new();
    state.mark_pty_output_bytes(b"plain output");
    assert_eq!(
        resolve_submit_encoding(&state, PtySubmitEncoding::BracketedPaste),
        PtySubmitEncoding::BracketedPaste
    );
    assert_eq!(
        resolve_submit_encoding(&state, PtySubmitEncoding::Raw),
        PtySubmitEncoding::Raw
    );
}

#[test]
fn question_on_screen_blocks_submit_including_the_interrupt_path() {
    let state = UserInputState::new();
    state.set_awaiting_user_input(true);
    assert!(state.is_awaiting_user_input());
    assert!(pty_submit_wait_blocked_for(&state, false));
    assert!(pty_submit_wait_blocked_for(&state, true));
}

#[test]
fn answering_the_question_unblocks_submit() {
    let state = UserInputState::new();
    state.set_awaiting_user_input(true);
    state.set_awaiting_user_input(false);
    assert!(!state.is_awaiting_user_input());
    assert!(!pty_submit_wait_blocked_for(&state, true));
}

#[test]
fn question_reported_again_does_not_extend_the_block_window() {
    let state = UserInputState::new();
    state.set_awaiting_user_input(true);
    let first = state.awaiting_since_ms.load(Ordering::Relaxed);
    state.set_awaiting_user_input(true);
    assert_eq!(state.awaiting_since_ms.load(Ordering::Relaxed), first);
}

#[test]
fn stale_question_stops_blocking_after_the_cap() {
    let state = UserInputState::new();
    state.set_awaiting_user_input(true);
    let stale = epoch_millis() - AWAITING_INPUT_MAX.as_millis() as u64 - 1;
    state.awaiting_since_ms.store(stale, Ordering::Relaxed);
    assert!(!state.is_awaiting_user_input());
}

#[test]
fn needs_input_is_published_as_its_own_activity_state() {
    let state = UserInputState::new();
    state.set_awaiting_user_input(true);
    assert_eq!(state.current_activity_state(), ActivityState::NeedsInput);
}

#[test]
fn user_typing_outranks_needs_input() {
    let state = UserInputState::new();
    state.set_awaiting_user_input(true);
    state.set_pending_line(b"partial answer");
    assert_eq!(state.current_activity_state(), ActivityState::UserTyping);
}

fn msg(id: i64, body: &str, created_at: u64) -> PendingMessage {
    PendingMessage {
        id,
        body: body.to_string(),
        created_at,
        interrupt: false,
    }
}

#[test]
fn prompt_delivery_carries_no_stamp() {
    let now = 1_700_000_000;
    let out = coalesce_batch(&[msg(1, "hello", now - 5)], now);
    assert_eq!(out, "hello");
}

#[test]
fn delayed_delivery_states_written_and_delivered_times() {
    let now = 1_700_000_000;
    let out = coalesce_batch(&[msg(1, "stale reply", now - 1408)], now);
    assert!(out.starts_with("[sidekar] delayed 23m28s · written "));
    assert!(out.contains(" · delivered "));
    assert!(out.ends_with("\nstale reply"));
}

#[test]
fn batch_is_pasted_once_with_a_header() {
    let now = 1_700_000_000;
    let out = coalesce_batch(
        &[msg(1, "first", now - 300), msg(2, "second", now - 10)],
        now,
    );
    assert!(out.starts_with("[sidekar] 2 messages arrived while this pane was busy"));
    assert!(out.contains("first"));
    assert!(out.contains("second"));
    // Only the one that actually waited is stamped.
    assert_eq!(out.matches("[sidekar] delayed").count(), 1);
}

#[test]
fn missing_enqueue_time_is_not_reported_as_a_delay() {
    let out = coalesce_batch(&[msg(1, "body", 0)], 1_700_000_000);
    assert_eq!(out, "body");
}

#[test]
fn batch_cutoff_stops_at_the_paste_ceiling() {
    let big = "x".repeat(MAX_BATCH_BYTES / 2 + 1);
    let pending = vec![msg(1, &big, 0), msg(2, &big, 0), msg(3, "tail", 0)];
    assert_eq!(batch_cutoff(&pending), 1);
}

#[test]
fn batch_cutoff_always_moves_at_least_one_message() {
    let oversized = "x".repeat(MAX_BATCH_BYTES * 2);
    assert_eq!(batch_cutoff(&[msg(1, &oversized, 0)]), 1);
}

#[test]
fn batch_cutoff_takes_everything_that_fits() {
    let pending = vec![msg(1, "a", 0), msg(2, "b", 0), msg(3, "c", 0)];
    assert_eq!(batch_cutoff(&pending), 3);
}

#[test]
fn human_delay_reads_as_minutes_past_a_minute() {
    assert_eq!(human_delay(0), "0s");
    assert_eq!(human_delay(59), "59s");
    assert_eq!(human_delay(60), "1m00s");
    assert_eq!(human_delay(1408), "23m28s");
}

#[test]
fn a_stashed_draft_does_not_gate_delivery() {
    let state = UserInputState::new();
    state.set_pending_line(b"half typed");
    assert!(state.stash_pending_line_for_inject());
    assert!(state.has_stashed_draft());
    // The stash clears only on a keystroke, so gating on it would deafen the
    // pane for as long as its human is away from the keyboard.
    assert!(!user_blocks_submit(&state));
    assert!(!pty_submit_wait_blocked_for(&state, false));
}

#[test]
fn active_typing_gates_delivery() {
    let state = UserInputState::new();
    state.set_pending_line(b"half typed");
    state.mark_activity();
    assert!(user_blocks_submit(&state));
}

#[test]
fn an_unanswered_question_gates_even_the_interrupt_path() {
    let state = UserInputState::new();
    state.set_awaiting_user_input_because(Some("question on screen".into()));
    assert!(user_blocks_submit(&state));
    assert!(pty_submit_wait_blocked_for(&state, true));
}

#[test]
fn a_working_agent_gates_only_the_plain_path() {
    let state = UserInputState::new();
    state.mark_pty_output();
    assert!(state.is_agent_working());
    assert!(
        !user_blocks_submit(&state),
        "the agent being busy is not the human's veto, so the deadline may override it"
    );
    assert!(pty_submit_wait_blocked_for(&state, false));
    assert!(!pty_submit_wait_blocked_for(&state, true));
}

#[test]
fn frame_parser_reads_a_bus_message() {
    let raw = br#"{"type":"bus_message","id":7,"body":"hi","created_at":42,"interrupt":true}"#;
    let msg = parse_bus_frame(raw).expect("frame should parse");
    assert_eq!(msg.id, 7);
    assert_eq!(msg.body, "hi");
    assert_eq!(msg.created_at, 42);
    assert!(msg.interrupt);
}

#[test]
fn frame_parser_defaults_optional_fields() {
    let msg = parse_bus_frame(br#"{"type":"bus_message","id":1}"#).expect("frame should parse");
    assert_eq!(msg.body, "");
    assert_eq!(msg.created_at, 0);
    assert!(!msg.interrupt);
}

#[test]
fn frame_parser_ignores_other_frames_and_junk() {
    assert!(parse_bus_frame(br#"{"type":"bus_ack","id":1}"#).is_none());
    assert!(parse_bus_frame(br#"{"type":"bus_message"}"#).is_none());
    assert!(parse_bus_frame(b"not json").is_none());
    assert!(parse_bus_frame(b"").is_none());
}

#[test]
fn split_frames_reassemble_across_reads() {
    // A frame arriving in pieces, as it does when the socket read times out
    // partway through. The accumulator must not surface it until the newline.
    let whole = br#"{"type":"bus_message","id":3,"body":"split","created_at":9}"#.to_vec();
    let (head, tail) = whole.split_at(20);

    let mut inbox: Vec<u8> = Vec::new();
    let mut pending: Vec<PendingMessage> = Vec::new();
    let mut drain = |inbox: &mut Vec<u8>, pending: &mut Vec<PendingMessage>| {
        while let Some(eol) = inbox.iter().position(|&b| b == b'\n') {
            let frame: Vec<u8> = inbox.drain(..=eol).collect();
            if let Some(msg) = parse_bus_frame(&frame[..eol]) {
                pending.push(msg);
            }
        }
    };

    inbox.extend_from_slice(head);
    drain(&mut inbox, &mut pending);
    assert!(pending.is_empty(), "half a frame is not a message");

    inbox.extend_from_slice(tail);
    inbox.push(b'\n');
    drain(&mut inbox, &mut pending);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, 3);
    assert_eq!(pending[0].body, "split");
}

#[test]
fn several_frames_in_one_read_all_surface() {
    let mut inbox: Vec<u8> = Vec::new();
    inbox.extend_from_slice(br#"{"type":"bus_message","id":1,"body":"a"}"#);
    inbox.push(b'\n');
    inbox.extend_from_slice(br#"{"type":"bus_ack","id":9}"#);
    inbox.push(b'\n');
    inbox.extend_from_slice(br#"{"type":"bus_message","id":2,"body":"b"}"#);
    inbox.push(b'\n');

    let mut pending: Vec<PendingMessage> = Vec::new();
    while let Some(eol) = inbox.iter().position(|&b| b == b'\n') {
        let frame: Vec<u8> = inbox.drain(..=eol).collect();
        if let Some(msg) = parse_bus_frame(&frame[..eol]) {
            pending.push(msg);
        }
    }
    assert_eq!(pending.iter().map(|m| m.id).collect::<Vec<_>>(), vec![1, 2]);
    assert!(inbox.is_empty());
}

// ---------------------------------------------------------------------------
// End-to-end: daemon socket -> client loop -> real PTY
// ---------------------------------------------------------------------------

/// A real pty pair. The client loop writes to the master; the test reads what
/// a wrapped agent would have received on the slave.
struct PtyPair {
    master: std::os::fd::OwnedFd,
    slave: std::os::fd::OwnedFd,
}

impl PtyPair {
    fn open() -> PtyPair {
        use std::os::fd::FromRawFd;
        let (mut m, mut s) = (0i32, 0i32);
        let rc = unsafe {
            libc::openpty(
                &mut m,
                &mut s,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty failed");
        // Raw slave: no echo, no line discipline rewriting of the paste.
        unsafe {
            let mut tio: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(s, &mut tio) == 0 {
                libc::cfmakeraw(&mut tio);
                libc::tcsetattr(s, libc::TCSANOW, &tio);
            }
        }
        PtyPair {
            master: unsafe { std::os::fd::OwnedFd::from_raw_fd(m) },
            slave: unsafe { std::os::fd::OwnedFd::from_raw_fd(s) },
        }
    }

    /// Read from the slave until `deadline`, returning whatever arrived.
    fn drain(&self, deadline: std::time::Duration) -> String {
        use std::os::fd::AsRawFd;
        let fd = self.slave.as_raw_fd();
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        let start = std::time::Instant::now();
        let mut out = Vec::new();
        while start.elapsed() < deadline {
            let mut buf = [0u8; 4096];
            let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n > 0 {
                out.extend_from_slice(&buf[..n as usize]);
            } else {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

fn frame(id: i64, body: &str, created_at: u64) -> String {
    serde_json::json!({
        "type": "bus_message",
        "id": id,
        "sender": "sender",
        "recipient": "receiver",
        "body": body,
        "created_at": created_at,
    })
    .to_string()
        + "\n"
}

#[test]
fn client_loop_pastes_a_backlog_once_and_acks_every_message() {
    use std::io::{BufRead, BufReader, Write};

    let _guard = crate::test_home_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = std::env::temp_dir().join(format!("sidekar-poller-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&home).expect("temp home");
    let old_home = std::env::var_os("HOME");
    // Safety: single-threaded section, HOME restored before returning.
    unsafe { std::env::set_var("HOME", &home) };

    let pty = PtyPair::open();
    let (mut daemon_side, client_side) =
        std::os::unix::net::UnixStream::pair().expect("socket pair");
    client_side
        .set_read_timeout(Some(std::time::Duration::from_millis(50)))
        .expect("read timeout");

    // Both frames are already buffered before the loop starts reading, so they
    // are picked up together and must leave as a single paste.
    let now = crate::message::epoch_secs();
    daemon_side
        .write_all(frame(101, "first message", now - 900).as_bytes())
        .expect("write frame 1");
    daemon_side
        .write_all(frame(102, "second message", now).as_bytes())
        .expect("write frame 2");
    daemon_side.flush().expect("flush");

    let input_state = Arc::new(UserInputState::new());
    assert!(input_state.is_idle(), "gate must start open");
    assert!(!input_state.is_agent_working());

    let (notice_tx, _notice_rx) = tokio::sync::mpsc::unbounded_channel::<PtyNotice>();
    let state_for_thread = Arc::clone(&input_state);
    let master = pty.master.try_clone().expect("dup master");
    let worker = std::thread::spawn(move || {
        let target = PtyTarget {
            fd: &master,
            input_state: &state_for_thread,
            notice_tx: &notice_tx,
            submit_encoding: PtySubmitEncoding::Raw,
            child_pid: std::process::id() as i32,
        };
        bus_client_loop(client_side, &target, None)
    });

    let pasted = pty.drain(std::time::Duration::from_millis(600));

    // One paste carrying both bodies, with the batch header and a delay stamp
    // on the message that actually waited.
    assert!(
        pasted.contains("first message") && pasted.contains("second message"),
        "both messages must reach the pane, got: {pasted:?}"
    );
    assert!(
        pasted.contains("2 messages arrived while this pane was busy"),
        "backlog must be coalesced into one paste, got: {pasted:?}"
    );
    assert_eq!(
        pasted.matches("[sidekar] delayed").count(),
        1,
        "only the delayed message carries a stamp, got: {pasted:?}"
    );
    assert_eq!(
        pasted.matches('\r').count(),
        1,
        "one submit for the whole batch, got: {pasted:?}"
    );

    // Every message in the batch is acked, so none can be redelivered.
    let mut acks = Vec::new();
    let mut reader = BufReader::new(daemon_side);
    let mut line = String::new();
    while acks.len() < 2 {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let v: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("type").and_then(|t| t.as_str()) == Some("bus_ack") {
            assert_eq!(v.get("ok").and_then(|o| o.as_bool()), Some(true));
            acks.push(v.get("id").and_then(|i| i.as_i64()).expect("ack id"));
        }
    }
    assert_eq!(acks, vec![101, 102]);

    POLLER_SHUTDOWN.store(true, Ordering::Relaxed);
    let _ = worker.join();
    POLLER_SHUTDOWN.store(false, Ordering::Relaxed);

    match old_home {
        Some(h) => unsafe { std::env::set_var("HOME", h) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_buffered_draft_defers_but_does_not_veto_forever() {
    let state = UserInputState::new();
    state.set_pending_line(b"half typed");
    // Cleared only by a keystroke, so it must never outrank the deadline:
    // text abandoned in an input box would otherwise deafen the pane.
    assert!(!user_blocks_submit(&state));
}

#[test]
fn active_typing_and_open_questions_still_veto() {
    let typing = UserInputState::new();
    typing.mark_activity();
    assert!(user_blocks_submit(&typing));

    let asked = UserInputState::new();
    asked.set_awaiting_user_input_because(Some("question on screen".into()));
    assert!(user_blocks_submit(&asked));
    assert!(pty_submit_wait_blocked_for(&asked, true));
}

#[test]
fn draft_is_lifted_out_and_handed_back_around_an_inject() {
    let pty = PtyPair::open();
    let state = UserInputState::new();
    state.set_pending_line(b"the draft I was writing");
    assert!(state.has_pending_line());

    // Lift out, exactly as deliver_to_pty does before pasting.
    let preview = state
        .prepare_pty_for_inject(pty.master.as_raw_fd())
        .expect("draft should be stashed");
    assert!(preview.contains("the draft I was writing"));
    assert!(!state.has_pending_line(), "line is cleared for the paste");

    // Hand it back afterwards.
    let draft = state
        .take_stashed_draft()
        .expect("draft should be recoverable");
    assert_eq!(draft, b"the draft I was writing");
    crate::pty::write_all_fd(pty.master.as_raw_fd(), &draft).expect("restore write");
    state.set_pending_line(&draft);

    assert!(state.has_pending_line(), "the human gets their line back");
    assert!(!state.has_stashed_draft(), "and nothing is left stashed");

    let seen = pty.drain(std::time::Duration::from_millis(200));
    assert!(
        seen.contains("the draft I was writing"),
        "the draft must be typed back into the pane, got {seen:?}"
    );
    assert!(seen.contains('\u{15}'), "and the line was cleared first");
}
