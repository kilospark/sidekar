use super::*;

fn feed_once(raw: &[u8]) -> Option<OscSignal> {
    OscStateDetector::new().feed(raw)
}

#[test]
fn braille_spinner_in_title_reads_as_working() {
    assert_eq!(
        feed_once("\x1b]0;\u{280B} Thinking…\x07".as_bytes()),
        Some(OscSignal::Working)
    );
}

#[test]
fn half_circle_spinner_in_title_reads_as_working() {
    assert_eq!(
        feed_once("\x1b]2;\u{25D0} Running tests\x07".as_bytes()),
        Some(OscSignal::Working)
    );
}

#[test]
fn hourglass_title_reads_as_working() {
    // Sidekar's own REPL parks an hourglass in the title while the agent runs.
    assert_eq!(
        feed_once("\x1b]0;\u{23F3} viper\x07".as_bytes()),
        Some(OscSignal::Working)
    );
}

#[test]
fn check_mark_title_reads_as_idle() {
    assert_eq!(
        feed_once("\x1b]0;\u{2705} viper\x07".as_bytes()),
        Some(OscSignal::Idle)
    );
}

#[test]
fn ordinary_title_text_yields_no_signal() {
    // A path or branch name says nothing about the turn; the spinner clock
    // must not be disturbed by an ordinary retitle.
    assert_eq!(feed_once(b"\x1b]0;~/src/sidekar\x07"), None);
    assert_eq!(feed_once(b"\x1b]2;viper (claude-sidekar-1)\x07"), None);
}

#[test]
fn string_terminator_is_accepted_as_well_as_bel() {
    assert_eq!(
        feed_once("\x1b]0;\u{280B} Thinking\x1b\\".as_bytes()),
        Some(OscSignal::Working)
    );
}

#[test]
fn progress_report_states_map_to_working_and_idle() {
    assert_eq!(feed_once(b"\x1b]9;4;1;40\x07"), Some(OscSignal::Working));
    assert_eq!(feed_once(b"\x1b]9;4;3;0\x07"), Some(OscSignal::Working));
    assert_eq!(feed_once(b"\x1b]9;4;0;0\x07"), Some(OscSignal::Idle));
}

#[test]
fn paused_progress_yields_no_signal() {
    // Paused work is neither running nor finished.
    assert_eq!(feed_once(b"\x1b]9;4;4;50\x07"), None);
}

#[test]
fn unrelated_osc_sequences_are_ignored() {
    // OSC 10/11 are colour queries; OSC 8 is a hyperlink.
    assert_eq!(feed_once(b"\x1b]11;rgb:0000/0000/0000\x07"), None);
    assert_eq!(feed_once(b"\x1b]8;;https://example.com\x07"), None);
}

#[test]
fn sequence_split_across_chunks_is_still_classified() {
    let mut d = OscStateDetector::new();
    assert_eq!(d.feed("\x1b]0;\u{280B} Thin".as_bytes()), None);
    assert_eq!(d.feed(b"king\x07"), Some(OscSignal::Working));
}

#[test]
fn escape_split_from_its_bracket_is_still_classified() {
    let mut d = OscStateDetector::new();
    assert_eq!(d.feed(b"output\x1b"), None);
    assert_eq!(
        d.feed("]0;\u{280B} go\x07".as_bytes()),
        Some(OscSignal::Working)
    );
}

#[test]
fn last_signal_in_a_chunk_wins() {
    // A repaint that starts a turn and finishes it within one read should
    // leave the agent idle, not working.
    let raw = "\x1b]0;\u{280B} Thinking\x07some output\x1b]0;\u{2705} done\x07";
    assert_eq!(feed_once(raw.as_bytes()), Some(OscSignal::Idle));
}

#[test]
fn unterminated_oversized_payload_is_abandoned() {
    let mut d = OscStateDetector::new();
    let mut raw = b"\x1b]0;".to_vec();
    raw.extend(std::iter::repeat_n(b'x', MAX_PAYLOAD + 16));
    assert_eq!(d.feed(&raw), None);
    assert!(!d.in_osc, "oversized payload should stop being tracked");
    // The detector stays usable for the next real sequence.
    assert_eq!(
        d.feed("\x1b]0;\u{280B} go\x07".as_bytes()),
        Some(OscSignal::Working)
    );
}

#[test]
fn plain_output_with_no_osc_yields_nothing() {
    assert_eq!(feed_once(b"just some regular agent output\n"), None);
}

#[test]
fn invalid_utf8_payload_is_ignored_without_panicking() {
    assert_eq!(feed_once(b"\x1b]0;\xff\xfe\x07"), None);
}

// ---- a title that stops animating --------------------------------------------

#[test]
fn a_spinner_title_settling_into_plain_text_ends_the_turn() {
    // Exactly what Claude Code does: `◐`/`◑` while a turn runs, then `✳` once
    // it finishes. `✳` is neither a spinner frame nor a check mark, so on its
    // own it says nothing — but as the frame after a spinner it says the
    // spinner stopped. Missing that pinned the spinner clock for its full three
    // minutes after every turn, and bus messages waited out the rest.
    let mut d = OscStateDetector::new();
    assert_eq!(
        d.feed("\x1b]0;\u{25D0} Claude Code\x07".as_bytes()),
        Some(OscSignal::Working)
    );
    assert_eq!(
        d.feed("\x1b]0;\u{25D1} Claude Code\x07".as_bytes()),
        Some(OscSignal::Working)
    );
    assert_eq!(
        d.feed("\x1b]0;\u{2733} Claude Code\x07".as_bytes()),
        Some(OscSignal::Idle),
        "the spinner stopping must clear the clock it armed"
    );
}

#[test]
fn a_plain_title_only_ends_a_turn_once() {
    // After the transition the title is simply plain again; repeating it must
    // not keep reporting Idle, and a later plain retitle means nothing.
    let mut d = OscStateDetector::new();
    d.feed("\x1b]0;\u{25D0} Claude Code\x07".as_bytes());
    assert_eq!(
        d.feed("\x1b]0;\u{2733} Claude Code\x07".as_bytes()),
        Some(OscSignal::Idle)
    );
    assert_eq!(d.feed("\x1b]0;\u{2733} Claude Code\x07".as_bytes()), None);
    assert_eq!(d.feed(b"\x1b]0;~/src/sidekar\x07"), None);
}

#[test]
fn a_plain_title_with_no_spinner_before_it_still_says_nothing() {
    // The existing guarantee, stated for the stateful detector: an ordinary
    // retitle that was not preceded by an animation is not a turn ending.
    let mut d = OscStateDetector::new();
    assert_eq!(d.feed("\x1b]0;\u{2733} Claude Code\x07".as_bytes()), None);
    assert_eq!(d.feed(b"\x1b]2;viper (claude-sidekar-1)\x07"), None);
}

#[test]
fn an_empty_title_does_not_end_a_turn() {
    // Empty carries no information: some CLIs blank the title between frames.
    let mut d = OscStateDetector::new();
    d.feed("\x1b]0;\u{25D0} Claude Code\x07".as_bytes());
    assert_eq!(d.feed(b"\x1b]0;\x07"), None);
    // ...and the animation is still considered running afterwards.
    assert_eq!(
        d.feed("\x1b]0;\u{2733} Claude Code\x07".as_bytes()),
        Some(OscSignal::Idle)
    );
}

#[test]
fn progress_reports_do_not_disturb_title_tracking() {
    // OSC 9;4 and the title are separate channels; a progress report arriving
    // between spinner frames must not make the next plain title read as idle.
    let mut d = OscStateDetector::new();
    assert_eq!(d.feed(b"\x1b]9;4;1;50\x07"), Some(OscSignal::Working));
    assert_eq!(d.feed("\x1b]0;\u{2733} Claude Code\x07".as_bytes()), None);
}
