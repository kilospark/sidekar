use super::*;

#[test]
fn process_input_bytes_emits_every_line_in_one_chunk() {
    let mut editor = LineEditor::with_history(Vec::new());
    let mut lines = Vec::new();
    // Paste-burst detection holds rapid-arrival chars in its buffer,
    // so tests must force-flush between logical lines to simulate the
    // idle-time gap that a real terminal provides between keystrokes.
    editor
        .process_input_bytes(b"first", |_, line| {
            lines.push(line);
        })
        .unwrap();
    editor.force_flush_paste_burst();
    editor
        .process_input_bytes(b"\n", |_, line| {
            lines.push(line);
        })
        .unwrap();
    editor
        .process_input_bytes(b"second", |_, line| {
            lines.push(line);
        })
        .unwrap();
    editor.force_flush_paste_burst();
    editor
        .process_input_bytes(b"\n", |_, line| {
            lines.push(line);
        })
        .unwrap();
    assert_eq!(lines[0].text, "first");
    assert_eq!(lines[1].text, "second");
    assert!(editor.buffer.is_empty());
}

#[test]
fn up_on_top_row_pulls_all_pending_followups_at_once() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.pending_followups.push_back(SubmittedLine {
        text: "queued a".into(),
        image_paths: Vec::new(),
    });
    editor.pending_followups.push_back(SubmittedLine {
        text: "queued b".into(),
        image_paths: Vec::new(),
    });
    editor.move_up_for_cols(80);
    assert_eq!(editor.buffer, "queued a\nqueued b");
    assert!(editor.pending_followups.is_empty());
}

#[test]
fn active_prompt_pollfds_compact_tunnel_only_fd() {
    let fds = build_input_pollfds(None, Some(42));
    assert_eq!(fds.len(), 1);
    assert_eq!(fds[0].fd, 42);
}

#[test]
fn paste_burst_flush_during_agent_routes_to_followups_not_submits() {
    // Regression: flush_paste_burst_if_due used to hard-code its submit
    // destination to pending_submits via submit_current_line. The background
    // input thread calls it on idle ticks while the agent is running, so any
    // fast typing with a trailing newline (paste-burst path with `submit=true`)
    // was delivered straight to the agent on the next turn instead of queueing
    // as a followup that the user pulls with ↑.
    //
    // Feed chars fast enough to activate the burst, append Enter while burst
    // is active (so it's held, not submitted), sleep past the idle timeout,
    // then call flush_paste_burst_if_due with the followup-routing callback
    // used by ActivePromptSession.
    let mut editor = LineEditor::with_history(Vec::new());
    editor
        .process_input_bytes(b"queued msg", |ed, line| {
            ed.queue_pending_followup(line);
        })
        .unwrap();
    editor
        .process_input_bytes(b"\n", |ed, line| {
            ed.queue_pending_followup(line);
        })
        .unwrap();
    std::thread::sleep(PasteBurst::recommended_active_flush_delay());
    editor.flush_paste_burst_if_due(|ed, line| {
        ed.queue_pending_followup(line);
    });
    assert_eq!(editor.pending_followups.len(), 1);
    assert_eq!(editor.pending_followups[0].text, "queued msg");
    assert!(
        editor.pending_submits.is_empty(),
        "paste-burst flush during agent must not leak into pending_submits"
    );
}

#[test]
fn active_prompt_submission_queues_followup_immediately() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor
        .process_input_bytes(b"next prompt", |ed, line| {
            ed.queue_pending_followup(line);
        })
        .unwrap();
    editor.force_flush_paste_burst();
    editor
        .process_input_bytes(b"\n", |ed, line| {
            ed.queue_pending_followup(line);
        })
        .unwrap();

    assert_eq!(editor.pending_followups.len(), 1);
    assert_eq!(editor.pending_followups[0].text, "next prompt");
    assert!(editor.buffer.is_empty());
}

#[test]
fn esc_detector_only_cancels_lone_escape_after_timeout() {
    let start = std::time::Instant::now();
    let mut detector = EscDetector::new();

    assert!(detector.feed_bytes(&[0x1b], start).is_empty());
    assert!(!detector.check_timeout(start + std::time::Duration::from_millis(20)));
    assert!(detector.check_timeout(start + std::time::Duration::from_millis(100)));

    let mut detector = EscDetector::new();
    let forwarded = detector.feed_bytes(&[0x1b, b'[', b'A'], start);
    assert_eq!(forwarded, vec![0x1b, b'[', b'A']);
    assert!(!detector.check_timeout(start + std::time::Duration::from_millis(100)));
}

#[test]
fn layout_wraps_prompt_and_text_by_display_width() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "abcdef".to_string();
    editor.cursor = editor.buffer.len();

    let layout = editor.compute_layout(4);
    assert_eq!(layout.rows, 2);
    assert_eq!(layout.end, CursorPos { row: 1, col: 4 });
    assert_eq!(layout.cursor, layout.end);
}

#[test]
fn combining_marks_move_as_one_grapheme() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "e\u{301}x".to_string();
    editor.cursor = editor.buffer.len();

    editor.move_left();
    assert_eq!(editor.cursor, "e\u{301}".len());

    editor.move_left();
    assert_eq!(editor.cursor, 0);

    editor.move_right();
    assert_eq!(editor.cursor, "e\u{301}".len());
}

#[test]
fn backspace_deletes_whole_grapheme_cluster() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "a👍🏽b".to_string();
    editor.cursor = "a👍🏽".len();

    editor.backspace();
    assert_eq!(editor.buffer, "ab");
    assert_eq!(editor.cursor, 1);
}

#[test]
fn delete_removes_combining_cluster_at_cursor() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "a\u{65}\u{301}b".to_string();
    editor.cursor = 1;

    editor.delete_at_cursor();
    assert_eq!(editor.buffer, "ab");
    assert_eq!(editor.cursor, 1);
}

#[test]
fn wide_graphemes_affect_layout_width() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "👍👍".to_string();
    editor.cursor = editor.buffer.len();

    let layout = editor.compute_layout(3);
    assert_eq!(layout.rows, 2);
    assert_eq!(layout.end.row, 1);
}

#[test]
fn up_down_move_between_wrapped_rows_before_history() {
    let mut editor = LineEditor::with_history(vec!["history-prev".to_string()]);
    editor.buffer = "abcdef".to_string();
    editor.cursor = 5;

    editor.move_up_for_cols(4);
    assert_eq!(editor.cursor, 1);
    assert_eq!(editor.buffer, "abcdef");

    editor.move_down_for_cols(4);
    assert_eq!(editor.cursor, 5);
    assert_eq!(editor.buffer, "abcdef");
}

#[test]
fn up_down_fall_back_to_history_at_row_boundaries() {
    let mut editor = LineEditor::with_history(vec!["history-prev".to_string()]);
    editor.buffer = "abcdef".to_string();
    editor.cursor = 1;

    editor.move_up_for_cols(4);
    assert_eq!(editor.buffer, "history-prev");

    editor.move_down_for_cols(4);
    assert_eq!(editor.buffer, "abcdef");
}

#[test]
fn ctrl_u_ctrl_k_and_yank_work() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "hello world".to_string();
    editor.cursor = 5;

    editor.kill_to_start();
    assert_eq!(editor.buffer, " world");
    assert_eq!(editor.cursor, 0);

    editor.yank();
    assert_eq!(editor.buffer, "hello world");
    assert_eq!(editor.cursor, 5);

    editor.kill_to_end();
    assert_eq!(editor.buffer, "hello");
    assert_eq!(editor.kill_buffer, " world");

    editor.yank();
    assert_eq!(editor.buffer, "hello world");
}

#[test]
fn ctrl_c_exits_repl() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "pending".to_string();
    editor.cursor = editor.buffer.len();
    let result = editor.feed_byte(0x03);
    assert!(matches!(result, LineEditResult::Eof));
    assert!(editor.buffer.is_empty());
    assert_eq!(editor.cursor, 0);
}

// --- Word movement ---

#[test]
fn word_backward_skips_whitespace_then_word() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "hello world".to_string();
    editor.cursor = editor.buffer.len(); // end
    assert_eq!(editor.beginning_of_previous_word(), 6); // "world"
    editor.cursor = 6;
    assert_eq!(editor.beginning_of_previous_word(), 0); // "hello"
}

#[test]
fn word_forward_skips_whitespace_then_word() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "hello world".to_string();
    editor.cursor = 0;
    assert_eq!(editor.end_of_next_word(), 5); // past "hello"
    editor.cursor = 5;
    assert_eq!(editor.end_of_next_word(), 11); // past "world"
}

#[test]
fn word_boundary_stops_at_separator_class_change() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "path/to/file".to_string();
    editor.cursor = editor.buffer.len();
    assert_eq!(editor.beginning_of_previous_word(), 8); // "file"
    editor.cursor = 8;
    assert_eq!(editor.beginning_of_previous_word(), 7); // "/"
    editor.cursor = 7;
    assert_eq!(editor.beginning_of_previous_word(), 5); // "to"
}

#[test]
fn delete_backward_word_kills_through_buffer() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "hello world".to_string();
    editor.cursor = editor.buffer.len();
    editor.delete_backward_word();
    assert_eq!(editor.buffer, "hello ");
    assert_eq!(editor.kill_buffer, "world");
    // Ctrl+Y recovers it
    editor.yank();
    assert_eq!(editor.buffer, "hello world");
}

#[test]
fn delete_forward_word_kills_through_buffer() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "hello world".to_string();
    editor.cursor = 0;
    editor.delete_forward_word();
    assert_eq!(editor.buffer, " world");
    assert_eq!(editor.kill_buffer, "hello");
}

#[test]
fn ctrl_w_deletes_backward_word() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "foo bar".to_string();
    editor.cursor = editor.buffer.len();
    editor.feed_byte(0x17); // Ctrl+W
    assert_eq!(editor.buffer, "foo ");
}

// --- Multiline ---

#[test]
fn wrapped_rows_handles_explicit_newlines() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "line1\nline2".to_string();
    let rows = editor.wrapped_rows(80);
    assert_eq!(rows.len(), 2);
    assert_eq!(&editor.buffer[rows[0].clone()], "line1");
    assert_eq!(&editor.buffer[rows[1].clone()], "line2");
}

#[test]
fn wrapped_rows_combines_newlines_and_wrapping() {
    let mut editor = LineEditor::with_history(Vec::new());
    // prompt "› " is 2 cols, so with cols=4 we get 2 usable chars on first row
    // "ab\ncd" → row0="ab", row1="cd" at cols=80
    // but "abcdef\ngh" at cols=4 → row0="ab", row1="cdef"(wraps), row2="gh"
    editor.buffer = "ab\ncdefgh".to_string();
    let rows = editor.wrapped_rows(6);
    // row 0: "ab" (prompt takes 2, "ab" takes 2, fits in 6)
    // row 1: starts after \n. cont prefix "· " takes 2 cols, "cdef" takes 4, total 6 → fits
    // row 2: "gh" wraps from row 1
    assert_eq!(&editor.buffer[rows[0].clone()], "ab");
    assert!(rows.len() >= 2);
    assert_eq!(&editor.buffer[rows[1].clone()], "cdef");
}

#[test]
fn kill_to_start_operates_on_current_line() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "first\nsecond".to_string();
    editor.cursor = 9; // mid "second" → "sec|ond"
    editor.kill_to_start();
    assert_eq!(editor.buffer, "first\nond");
    assert_eq!(editor.kill_buffer, "sec");
    assert_eq!(editor.cursor, 6); // at start of "ond"
}

#[test]
fn kill_to_end_operates_on_current_line() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "first\nsecond".to_string();
    editor.cursor = 2; // "fi|rst"
    editor.kill_to_end();
    assert_eq!(editor.buffer, "fi\nsecond");
    assert_eq!(editor.kill_buffer, "rst");
}

#[test]
fn kill_to_start_at_bol_kills_preceding_newline() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "first\nsecond".to_string();
    editor.cursor = 6; // start of "second"
    editor.kill_to_start();
    assert_eq!(editor.buffer, "firstsecond");
    assert_eq!(editor.cursor, 5);
}

#[test]
fn kill_to_end_at_eol_kills_trailing_newline() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "first\nsecond".to_string();
    editor.cursor = 5; // end of "first"
    editor.kill_to_end();
    assert_eq!(editor.buffer, "firstsecond");
}

#[test]
fn ctrl_a_ctrl_e_navigate_current_line() {
    let mut editor = LineEditor::with_history(Vec::new());
    editor.buffer = "first\nsecond".to_string();
    editor.cursor = 9; // mid "second"
    assert_eq!(editor.beginning_of_current_line(), 6);
    assert_eq!(editor.end_of_current_line(), 12);
}

// --- Bracketed paste ---

#[test]
fn bracketed_paste_inserts_text() {
    let mut editor = LineEditor::with_history(Vec::new());
    // Begin paste marker: ESC[200~
    for &b in b"\x1b[200~" {
        editor.feed_byte(b);
    }
    assert!(editor.paste_buffer.is_some());
    // Paste content
    for &b in b"pasted text" {
        editor.feed_byte(b);
    }
    // End paste marker: ESC[201~
    for &b in b"\x1b[201~" {
        editor.feed_byte(b);
    }
    assert!(editor.paste_buffer.is_none());
    assert_eq!(editor.buffer, "pasted text");
    assert_eq!(editor.cursor, "pasted text".len());
}

#[test]
fn bracketed_paste_with_newlines() {
    let mut editor = LineEditor::with_history(Vec::new());
    for &b in b"\x1b[200~line1\rline2\x1b[201~" {
        editor.feed_byte(b);
    }
    assert_eq!(editor.buffer, "line1\nline2");
}

#[test]
fn bracketed_paste_ignores_escape_sequences_in_content() {
    let mut editor = LineEditor::with_history(Vec::new());
    // Paste content contains ESC[A (arrow up) — should be buffered, not dispatched
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend_from_slice(b"before\x1b[Aafter");
    bytes.extend_from_slice(b"\x1b[201~");
    for &b in &bytes {
        editor.feed_byte(b);
    }
    assert_eq!(editor.buffer, "before\x1b[Aafter");
}

#[test]
fn bracketed_paste_with_utf8() {
    let mut editor = LineEditor::with_history(Vec::new());
    let content = "héllo wörld";
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend_from_slice(content.as_bytes());
    bytes.extend_from_slice(b"\x1b[201~");
    for &b in &bytes {
        editor.feed_byte(b);
    }
    assert_eq!(editor.buffer, content);
}

fn paste_bytes(editor: &mut LineEditor, content: &[u8]) {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend_from_slice(content);
    bytes.extend_from_slice(b"\x1b[201~");
    for &b in &bytes {
        editor.feed_byte(b);
    }
}

#[test]
fn large_paste_inserts_placeholder_and_expands_on_submit() {
    let mut editor = LineEditor::with_history(Vec::new());
    let payload = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5);
    paste_bytes(&mut editor, payload.as_bytes());
    let placeholder = format!("[Pasted Content {} chars]", payload.chars().count());
    assert_eq!(editor.buffer, placeholder);
    assert_eq!(editor.cursor, placeholder.len());
    assert_eq!(editor.pending_pastes.len(), 1);

    let expanded = editor.expand_pending_pastes(editor.buffer.clone());
    assert_eq!(expanded, payload);
}

#[test]
fn large_paste_at_threshold_inserts_raw_text() {
    let mut editor = LineEditor::with_history(Vec::new());
    let payload = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD);
    paste_bytes(&mut editor, payload.as_bytes());
    assert_eq!(editor.buffer, payload);
    assert!(editor.pending_pastes.is_empty());
}

#[test]
fn duplicate_large_paste_size_gets_suffixed_placeholder() {
    let mut editor = LineEditor::with_history(Vec::new());
    let payload = "y".repeat(LARGE_PASTE_CHAR_THRESHOLD + 1);
    paste_bytes(&mut editor, payload.as_bytes());
    paste_bytes(&mut editor, payload.as_bytes());
    let base = format!("[Pasted Content {} chars]", payload.chars().count());
    assert_eq!(editor.buffer, format!("{base}{base} #2"));
    assert_eq!(editor.pending_pastes.len(), 2);

    let expanded = editor.expand_pending_pastes(editor.buffer.clone());
    assert_eq!(expanded, format!("{payload}{payload}"));
}

#[test]
fn backspace_removes_large_paste_placeholder_atomically() {
    let mut editor = LineEditor::with_history(Vec::new());
    let payload = "z".repeat(LARGE_PASTE_CHAR_THRESHOLD + 3);
    paste_bytes(&mut editor, payload.as_bytes());
    assert!(!editor.pending_pastes.is_empty());
    editor.backspace();
    assert!(editor.buffer.is_empty());
    assert_eq!(editor.cursor, 0);
    assert!(editor.pending_pastes.is_empty());
}

#[test]
fn delete_forward_removes_large_paste_placeholder_atomically() {
    let mut editor = LineEditor::with_history(Vec::new());
    let payload = "a".repeat(LARGE_PASTE_CHAR_THRESHOLD + 7);
    paste_bytes(&mut editor, payload.as_bytes());
    editor.cursor = 0;
    editor.delete_at_cursor();
    assert!(editor.buffer.is_empty());
    assert!(editor.pending_pastes.is_empty());
}

#[test]
fn kill_range_prunes_pending_pastes() {
    let mut editor = LineEditor::with_history(Vec::new());
    let payload = "b".repeat(LARGE_PASTE_CHAR_THRESHOLD + 2);
    paste_bytes(&mut editor, payload.as_bytes());
    let len = editor.buffer.len();
    editor.kill_range(0..len);
    assert!(editor.pending_pastes.is_empty());
}
