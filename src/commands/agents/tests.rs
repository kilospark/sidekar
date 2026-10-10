use super::*;

const NOW: u64 = 1_000_000;

fn detail(state: ActivityState, at: u64) -> ActivityDetail {
    ActivityDetail {
        state,
        at,
        reason: Some("screen changed 3ms ago".into()),
        settled_at: None,
        seen_at: None,
    }
}

fn row(nick: &str, attention: Attention) -> Row {
    Row {
        name: format!("claude-/tmp/proj-{nick}"),
        nick: nick.into(),
        harness: "claude".into(),
        channel: "proj".into(),
        attention,
        detail: String::new(),
        spawned_by: None,
        you: false,
        host: None,
    }
}

// ---- classification --------------------------------------------------------

#[test]
fn a_dead_process_is_dead_whatever_it_last_said() {
    // The registration outlives a crash; the last report would say "working".
    let d = detail(ActivityState::AgentWorking, NOW);
    assert_eq!(classify(false, Some(&d), NOW).0, Attention::Dead);
}

#[test]
fn a_question_on_screen_needs_input() {
    let mut d = detail(ActivityState::NeedsInput, NOW);
    d.reason = Some("question on screen".into());
    let (a, why) = classify(true, Some(&d), NOW);
    assert_eq!(a, Attention::NeedsInput);
    assert_eq!(why, "question on screen");
}

#[test]
fn a_finished_turn_nobody_looked_at_is_done() {
    // Settled after the human was last present: the result is waiting for them.
    let mut d = detail(ActivityState::Idle, NOW);
    d.settled_at = Some(NOW - 120);
    d.seen_at = Some(NOW - 600);
    let (a, why) = classify(true, Some(&d), NOW);
    assert_eq!(a, Attention::DoneUnseen);
    assert!(why.contains("2m"), "{why}");
}

#[test]
fn a_finished_turn_someone_has_seen_is_just_idle() {
    let mut d = detail(ActivityState::Idle, NOW);
    d.settled_at = Some(NOW - 120);
    d.seen_at = Some(NOW - 30);
    assert_eq!(classify(true, Some(&d), NOW).0, Attention::Idle);
}

#[test]
fn a_live_agent_that_stopped_reporting_is_stale() {
    // Every wrapper republishes at least every 30s, so a long silence from a
    // live process is the one state worth a person checking by hand.
    let d = detail(ActivityState::AgentWorking, NOW - ACTIVITY_STALE_SECS - 1);
    let (a, why) = classify(true, Some(&d), NOW);
    assert_eq!(a, Attention::Stale);
    assert!(why.starts_with("no report for"), "{why}");
}

#[test]
fn staleness_is_the_same_threshold_bus_explain_uses() {
    // One constant, so the view and `bus explain` cannot disagree about when a
    // refreshing state has gone quiet too long.
    let fresh = detail(ActivityState::AgentWorking, NOW - ACTIVITY_STALE_SECS);
    assert_eq!(classify(true, Some(&fresh), NOW).0, Attention::Working);
}

#[test]
fn an_agent_idle_for_a_long_time_is_idle_not_stale() {
    // Idle is published once and never refreshed, so its timestamp freezes the
    // moment the agent goes quiet. Judging it by age called every agent stale
    // after a minute of doing nothing — the first live run showed exactly that.
    let mut d = detail(ActivityState::Idle, NOW - 3_600);
    d.settled_at = Some(NOW - 3_600);
    d.seen_at = Some(NOW - 1);
    let (a, why) = classify(true, Some(&d), NOW);
    assert_eq!(a, Attention::Idle, "{why}");
    assert!(why.contains("1h"), "{why}");
}

#[test]
fn done_unseen_survives_a_long_wait_too() {
    // The case this view exists for: a result that has sat there for an hour
    // is the one most worth showing, and it must not decay into "stale".
    let mut d = detail(ActivityState::Idle, NOW - 3_600);
    d.settled_at = Some(NOW - 3_600);
    d.seen_at = Some(NOW - 7_200);
    assert_eq!(classify(true, Some(&d), NOW).0, Attention::DoneUnseen);
}

#[test]
fn needs_input_that_stopped_refreshing_is_stale() {
    let d = detail(ActivityState::NeedsInput, NOW - ACTIVITY_STALE_SECS - 1);
    assert_eq!(classify(true, Some(&d), NOW).0, Attention::Stale);
}

#[test]
fn a_malformed_nick_is_cut_rather_than_breaking_the_table() {
    // A real nick is one word. The first live run showed a 60-character
    // ciphertext in this column; it should be visible as wrong, not wreck
    // every other column's alignment.
    let long = "$encrypted$J3UOEGnys8GkJwfbbkdrDiVBGrbTTaDwwUIeGXkUUQS2oA==";
    let mut r = row("x", Attention::Idle);
    r.nick = long.into();
    let out = render(&[r]);
    let line = out.lines().nth(1).unwrap();
    // Derived from the constant, not hand-counted: the first version of this
    // test miscounted by one.
    let expected: String = long
        .chars()
        .take(NICK_MAX)
        .chain(std::iter::once('…'))
        .collect();
    assert!(
        line.contains(&expected),
        "expected {expected:?} in {line:?}"
    );
    assert!(
        !line.contains("QS2oA=="),
        "the whole value leaked through: {line}"
    );
}

#[test]
fn working_carries_the_detectors_own_reason() {
    let d = detail(ActivityState::AgentWorking, NOW);
    let (a, why) = classify(true, Some(&d), NOW);
    assert_eq!(a, Attention::Working);
    assert_eq!(why, "screen changed 3ms ago");
}

#[test]
fn an_agent_that_never_reported_is_stale_not_idle() {
    // Calling it idle would invite a message into something we know nothing about.
    assert_eq!(classify(true, None, NOW).0, Attention::Stale);
}

// ---- ordering --------------------------------------------------------------

#[test]
fn the_view_puts_what_needs_you_first() {
    let rows = vec![
        row("dead1", Attention::Dead),
        row("idle1", Attention::Idle),
        row("work1", Attention::Working),
        row("done1", Attention::DoneUnseen),
        row("ask1", Attention::NeedsInput),
    ];
    let out = render(&rows);
    let pos = |n: &str| out.find(n).unwrap_or_else(|| panic!("{n} missing:\n{out}"));
    assert!(pos("ask1") < pos("done1"));
    assert!(pos("done1") < pos("work1"));
    assert!(pos("work1") < pos("idle1"));
    assert!(pos("idle1") < pos("dead1"));
}

#[test]
fn the_tally_leads_with_what_needs_you() {
    let rows = vec![
        row("a", Attention::Working),
        row("b", Attention::NeedsInput),
        row("c", Attention::DoneUnseen),
    ];
    let first = render(&rows).lines().next().unwrap().to_string();
    assert_eq!(first, "3 agents: 1 needs you, 1 done, 1 working");
}

#[test]
fn you_and_your_spawner_are_marked() {
    let mut me = row("walrus", Attention::Working);
    me.you = true;
    let mut child = row("moray", Attention::Idle);
    child.spawned_by = Some("walrus".into());
    let out = render(&[me, child]);
    assert!(out.contains("walrus (you)"), "{out}");
    assert!(out.contains("spawned by walrus"), "{out}");
}

#[test]
fn no_agents_says_so() {
    assert_eq!(render(&[]), "No agents running.\n");
}

// ---- naming ----------------------------------------------------------------

#[test]
fn the_harness_is_read_back_from_the_name() {
    assert_eq!(
        harness_of("claude-/Users/k/src/sidekar-1", Some("sidekar")),
        "claude"
    );
    assert_eq!(
        harness_of("codex-/Users/k/src/sidekar-2", Some("sidekar")),
        "codex"
    );
}

#[test]
fn a_hyphenated_harness_is_not_cut_short() {
    // Splitting on the first '-' alone would call this "cursor".
    assert_eq!(
        harness_of("cursor-agent-/Users/k/src/sidekar-1", Some("sidekar")),
        "cursor-agent"
    );
}

#[test]
fn a_repl_is_named_as_one() {
    assert_eq!(
        harness_of("sidekar-repl-/Users/k/src/x-1", Some("sidekar-repl")),
        "repl"
    );
}

#[test]
fn a_channel_shows_as_its_last_component() {
    assert_eq!(short_channel("/Users/k/src/sidekar"), "sidekar");
    assert_eq!(short_channel("/Users/k/src/sidekar/"), "sidekar");
    assert_eq!(short_channel("local"), "local");
}

#[test]
fn durations_read_at_a_glance() {
    assert_eq!(ago(40), "40s");
    assert_eq!(ago(180), "3m");
    assert_eq!(ago(7_200), "2h");
    assert_eq!(ago(3 * 86_400), "3d");
}

// ---- watch -----------------------------------------------------------------

#[test]
fn a_redraw_never_blanks_the_screen() {
    // Clearing the whole screen first would flash empty on every refresh.
    let out = redraw("a\nb\n");
    assert!(
        !out.contains("\x1b[2J"),
        "full clear causes flicker: {out:?}"
    );
    assert!(out.starts_with("\x1b[H"));
}

#[test]
fn a_shorter_frame_leaves_no_ghost_rows() {
    // Each line clears its own tail, and everything below the frame is cleared,
    // so a row that disappeared between refreshes does not linger.
    let out = redraw("one\ntwo\n");
    assert_eq!(out.matches("\x1b[K").count(), 2);
    assert!(out.ends_with("\x1b[J"));
}

#[test]
fn watch_takes_an_interval_or_defaults_to_two_seconds() {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(watch_interval(&s(&[])).unwrap(), None);
    assert_eq!(
        watch_interval(&s(&["--watch"])).unwrap().unwrap().as_secs(),
        2
    );
    assert_eq!(
        watch_interval(&s(&["--watch", "5"]))
            .unwrap()
            .unwrap()
            .as_secs(),
        5
    );
    assert_eq!(
        watch_interval(&s(&["--watch=7"]))
            .unwrap()
            .unwrap()
            .as_secs(),
        7
    );
}

#[test]
fn watch_refuses_nonsense_and_unknown_flags() {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert!(
        watch_interval(&s(&["--watch", "0"])).is_err(),
        "0 would redraw continuously"
    );
    assert!(watch_interval(&s(&["--watch=fast"])).is_err());
    // The silent-flag bug, not repeated here.
    assert!(watch_interval(&s(&["--wach"])).is_err());
}
