use super::*;

#[test]
fn detects_claude_permission_option_list() {
    let screen = "\
Bash(rm -rf build/)
Do you want to proceed?
❯ 1. Yes
  2. Yes, and don't ask again for rm commands
  3. No, and tell Claude what to do differently
";
    assert!(looks_like_question(screen));
}

#[test]
fn detects_option_list_without_prose_marker() {
    let screen = "\
Select a model
❯ 1. Opus
  2. Sonnet
  3. Haiku
";
    assert!(looks_like_question(screen));
}

#[test]
fn detects_inline_yes_no() {
    assert!(looks_like_question("Overwrite existing config (y/n)? "));
    assert!(looks_like_question("Install the skill? [Y/n]"));
}

#[test]
fn detects_press_enter() {
    assert!(looks_like_question("Press Enter to continue..."));
}

#[test]
fn idle_composer_is_not_a_question() {
    let screen = "\
✻ Welcome to Claude Code

>
  ? for shortcuts
";
    assert!(!looks_like_question(screen));
}

#[test]
fn ordinary_agent_prose_is_not_a_question() {
    let screen = "\
I updated three files and ran the tests. All 42 pass.
The change moves the retry loop into a helper so both callers share it.
";
    assert!(!looks_like_question(screen));
}

#[test]
fn markdown_blockquote_list_is_not_a_question() {
    let screen = "\
Here is the plan:
> 1. Read the config
> 2. Apply the migration
> 3. Restart the daemon
";
    assert!(!looks_like_question(screen));
}

#[test]
fn plain_numbered_list_without_cursor_is_not_a_question() {
    let screen = "\
Steps:
1. Read the config
2. Apply the migration
3. Restart the daemon
";
    assert!(!looks_like_question(screen));
}

#[test]
fn single_option_is_not_a_question() {
    assert!(!looks_like_question("❯ 1. Only choice\n"));
}

#[test]
fn question_scrolled_out_of_window_is_ignored() {
    let mut screen = String::from("Do you want to proceed?\n");
    for i in 0..INSPECT_LINES + 4 {
        screen.push_str(&format!("output line {i}\n"));
    }
    assert!(!looks_like_question(&screen));
}

#[test]
fn detector_trims_tail_and_still_matches() {
    let mut detector = WaitingDetector::new();
    detector.feed_text(&"filler line\n".repeat(600));
    assert!(detector.tail.len() <= TAIL_CAPACITY + 16);
    assert!(!detector.is_question_on_screen());

    detector.feed_text("Apply this change? (y/n)\n");
    assert!(detector.is_question_on_screen());

    detector.clear();
    assert!(!detector.is_question_on_screen());
}

#[test]
fn detector_handles_multibyte_trim() {
    let mut detector = WaitingDetector::new();
    detector.feed_text(&"❯ ▶ ➤ multibyte filler\n".repeat(400));
    detector.feed_text("Are you sure?\n");
    assert!(detector.is_question_on_screen());
}

#[test]
fn detects_screen_resets() {
    assert!(resets_screen(b"\x1b[2J\x1b[H"));
    assert!(resets_screen(b"prefix\x1b[3Jsuffix"));
    assert!(resets_screen(b"\x1b[?1049h"));
    assert!(resets_screen(b"\x1b[?1049l"));
}

#[test]
fn ordinary_output_is_not_a_screen_reset() {
    assert!(!resets_screen(b"\x1b[1;32mhello\x1b[0m\r\n"));
    assert!(!resets_screen(b"\x1b[2K"));
    assert!(!resets_screen(b""));
}
