use super::*;

#[test]
fn tracks_bracketed_paste_toggle() {
    let mode = TerminalInputMode::new();
    assert_eq!(mode.bracketed_paste_observed(), None);

    mode.feed(b"\x1b[?2004h");
    assert_eq!(mode.bracketed_paste_observed(), Some(true));

    mode.feed(b"hello \x1b[?2004l world");
    assert_eq!(mode.bracketed_paste_observed(), Some(false));
}

#[test]
fn tracks_alternate_screen_and_cursor() {
    let mode = TerminalInputMode::new();
    mode.feed(b"\x1b[?1049h\x1b[?25l");
    assert!(mode.alternate_screen());
    assert!(mode.cursor_hidden());

    mode.feed(b"\x1b[?25h\x1b[?1049l");
    assert!(!mode.alternate_screen());
    assert!(!mode.cursor_hidden());
}

#[test]
fn tracks_application_cursor_keys() {
    let mode = TerminalInputMode::new();
    assert!(!mode.application_cursor_keys());
    mode.feed(b"\x1b[?1h");
    assert!(mode.application_cursor_keys());
    mode.feed(b"\x1b[?1l");
    assert!(!mode.application_cursor_keys());
}

#[test]
fn handles_multi_param_private_modes() {
    let mode = TerminalInputMode::new();
    mode.feed(b"\x1b[?1;2004h");
    assert!(mode.application_cursor_keys());
    assert!(mode.bracketed_paste());
}

#[test]
fn carries_split_escape_across_chunks() {
    let mode = TerminalInputMode::new();
    mode.feed(b"text\x1b[?20");
    assert_eq!(mode.bracketed_paste_observed(), None);
    mode.feed(b"04h");
    assert_eq!(mode.bracketed_paste_observed(), Some(true));
}

#[test]
fn kitty_push_and_pop_restores_flags() {
    let mode = TerminalInputMode::new();
    assert!(!mode.supports_modified_enter());

    mode.feed(b"\x1b[>1u");
    assert_eq!(mode.kitty_flags(), 1);
    assert!(mode.supports_modified_enter());

    mode.feed(b"\x1b[>15u");
    assert_eq!(mode.kitty_flags(), 15);

    mode.feed(b"\x1b[<1u");
    assert_eq!(mode.kitty_flags(), 1);

    mode.feed(b"\x1b[<1u");
    assert_eq!(mode.kitty_flags(), 0);
    assert!(!mode.supports_modified_enter());
}

#[test]
fn kitty_set_modes_or_and_and_out() {
    let mode = TerminalInputMode::new();
    mode.feed(b"\x1b[=5;1u");
    assert_eq!(mode.kitty_flags(), 5);

    mode.feed(b"\x1b[=2;2u");
    assert_eq!(mode.kitty_flags(), 7);

    mode.feed(b"\x1b[=1;3u");
    assert_eq!(mode.kitty_flags(), 6);
}

#[test]
fn preamble_reasserts_active_modes_in_order() {
    let mode = TerminalInputMode::new();
    mode.feed(b"\x1b[?1049h\x1b[>3u\x1b[?1h\x1b[?2004h\x1b[?25l");

    let preamble = mode.preamble();
    let text = String::from_utf8(preamble).unwrap();
    assert_eq!(text, "\x1b[?1049h\x1b[=3;1u\x1b[?1h\x1b[?2004h\x1b[?25l");
}

#[test]
fn preamble_is_empty_for_untouched_terminal() {
    let mode = TerminalInputMode::new();
    mode.feed(b"plain output with no escapes");
    assert!(mode.preamble().is_empty());
}

#[test]
fn parse_tail_is_bounded() {
    let mode = TerminalInputMode::new();
    let long: Vec<u8> = std::iter::once(0x1b)
        .chain(std::iter::once(b'['))
        .chain(std::iter::repeat_n(b'1', MAX_PARSE_TAIL * 2))
        .collect();
    mode.feed(&long);
    assert!(mode.parse_tail.lock().unwrap().len() <= MAX_PARSE_TAIL);
}

#[test]
fn ignores_non_csi_escapes() {
    let mode = TerminalInputMode::new();
    mode.feed(b"\x1b]0;title\x07\x1b(B\x1b[?2004h");
    assert_eq!(mode.bracketed_paste_observed(), Some(true));
}
