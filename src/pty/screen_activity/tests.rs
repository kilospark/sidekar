use super::*;

/// One whole frame the way Claude Code draws it at idle: erase the screen, go
/// home, redraw every line. `shimmer` picks which colour the status line gets,
/// which is the only thing that changes from one idle frame to the next.
fn idle_frame(shimmer: u8) -> Vec<u8> {
    let colour = if shimmer % 2 == 0 {
        "\x1b[91m"
    } else {
        "\x1b[37m"
    };
    format!(
        "\x1b[2J\x1b[H\
         Claude Code\r\n\
         \u{23fa} Hi! What would you like to work on?\r\n\
         \x1b[12G{colour}\u{273b} Churned for 2s \u{b7} done\x1b[39m\r\n\
         \u{276f} "
    )
    .into_bytes()
}

#[test]
fn an_idle_repaint_is_not_a_change() {
    // The bug this module fixes: a new colour on every frame, the same text.
    // Byte-for-byte these frames differ; what they draw does not.
    let mut s = ScreenActivity::new(120, 32);
    s.feed(&idle_frame(0), 1);
    assert!(s.settle(), "the first screen an agent draws is activity");
    for f in 1..50 {
        s.feed(&idle_frame(f), 1 + f as u64 * 200);
        assert!(
            !s.settle(),
            "idle frame {f} was counted as the agent working"
        );
    }
}

#[test]
fn new_text_is_a_change() {
    let mut s = ScreenActivity::new(120, 32);
    s.feed(&idle_frame(0), 1);
    s.settle();
    s.feed(
        b"\x1b[2J\x1b[HClaude Code\r\n\xe2\x8f\xba Writing the patch now\r\n",
        200,
    );
    assert!(
        s.settle(),
        "an agent that drew new text did not register as working"
    );
}

#[test]
fn a_frame_split_across_reads_is_judged_whole() {
    // Why settling matters. A frame that lands across two reads leaves the
    // screen half-drawn at the boundary; judged there, it always differs.
    // Judged once the frame is complete, it matches the last one.
    let mut s = ScreenActivity::new(120, 32);
    s.feed(&idle_frame(0), 1);
    s.settle();
    let frame = idle_frame(1);
    let (a, b) = frame.split_at(frame.len() / 2);
    s.feed(a, 200);
    s.feed(b, 201); // no settle between: the reads came back to back
    assert!(
        !s.settle(),
        "a whole idle frame, delivered in two reads, read as a change"
    );
}

#[test]
fn judging_mid_frame_would_see_a_change_that_is_not_there() {
    // The failure mode settling avoids, pinned so nobody "simplifies" the
    // settle away and compares on every read.
    let mut s = ScreenActivity::new(120, 32);
    s.feed(&idle_frame(0), 1);
    s.settle();
    let frame = idle_frame(1);
    s.feed(&frame[..frame.len() / 2], 200);
    assert!(
        s.settle(),
        "a half-drawn frame should differ from a whole one"
    );
}

#[test]
fn a_cycling_spinner_is_still_work() {
    // Real working indicators change the glyph, not just its colour, so they
    // must keep registering. Otherwise the fix would trade one wrong answer for
    // the opposite one.
    let mut s = ScreenActivity::new(120, 32);
    for (t, glyph) in ["\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}"]
        .iter()
        .enumerate()
    {
        s.feed(
            format!("\x1b[2J\x1b[H{glyph} Thinking\u{2026}").as_bytes(),
            t as u64 * 100 + 1,
        );
        assert!(s.settle(), "spinner frame {glyph} did not register as work");
    }
}

#[test]
fn settling_with_nothing_new_is_not_a_change() {
    let mut s = ScreenActivity::new(120, 32);
    s.feed(&idle_frame(0), 1);
    s.settle();
    assert!(!s.settle(), "a settle with no new output reported a change");
    assert_eq!(s.unsettled_for_ms(10_000), 0);
}

#[test]
fn unsettled_time_grows_until_a_settle_and_then_resets() {
    // What catches an agent streaming too fast to ever pause.
    let mut s = ScreenActivity::new(120, 32);
    assert_eq!(s.unsettled_for_ms(1_000), 0);
    s.feed(b"token", 1_000);
    s.feed(b" token", 1_040);
    s.feed(b" token", 1_600);
    assert_eq!(
        s.unsettled_for_ms(1_600),
        600,
        "measured from the first unsettled byte"
    );
    s.settle();
    assert_eq!(s.unsettled_for_ms(1_700), 0);
}

#[test]
fn empty_input_does_not_dirty_the_screen() {
    let mut s = ScreenActivity::new(120, 32);
    s.feed(b"", 5);
    assert_eq!(
        s.unsettled_for_ms(1_000),
        0,
        "empty input started the unsettled clock"
    );
}

#[test]
fn resizing_is_tolerated_and_a_zero_size_is_ignored() {
    // A zero size is what an unreadable window reports; applying it would give
    // the model no rows at all and hide every change.
    let mut s = ScreenActivity::new(120, 32);
    s.resize(0, 0);
    assert_eq!(s.parser.screen().size(), (32, 120));
    s.resize(80, 24);
    assert_eq!(s.parser.screen().size(), (24, 80));
    s.feed(&idle_frame(0), 1);
    s.settle();
}
