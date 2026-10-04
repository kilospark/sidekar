use super::*;
use std::env;

/// Run `f` against a throwaway broker database.
///
/// Safe only because every broker call `Presence` makes goes through `open()`,
/// which re-reads the database path on each call and so honours the swapped
/// HOME. The thread-local `with_cached_conn` does not: it keeps whatever
/// database it first opened, and cargo reuses test threads, so a test that
/// reached it would write fake agents onto the developer's real bus. Don't
/// call activity functions from here without isolating that cache first.
fn with_test_db(f: impl FnOnce()) {
    let _guard = crate::test_home_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let old_home = env::var_os("HOME");
    let home = env::temp_dir().join(format!(
        "sidekar-presence-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&home).unwrap();
    // SAFETY: serialized by test_home_lock and restored before returning.
    unsafe { env::set_var("HOME", &home) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    match old_home {
        Some(h) => unsafe { env::set_var("HOME", h) },
        None => unsafe { env::remove_var("HOME") },
    }
    let _ = std::fs::remove_dir_all(&home);
    if let Err(p) = result {
        std::panic::resume_unwind(p);
    }
}

fn registration(name: &str) -> Registration {
    Registration {
        name: name.to_string(),
        nick: "otter".into(),
        channel: "/tmp/proj".into(),
        pane: format!("test-{name}"),
        agent_type: "sidekar",
        history: None,
    }
}

fn on_bus(name: &str) -> bool {
    broker::list_agents(None)
        .unwrap_or_default()
        .iter()
        .any(|a| a.id.name == name)
}

// ---- naming ----------------------------------------------------------------

#[test]
fn the_first_name_is_one() {
    assert_eq!(first_free("claude-x", &HashSet::new()), "claude-x-1");
}

#[test]
fn a_taken_name_moves_to_the_next_free_number() {
    let taken: HashSet<String> = ["claude-x-1", "claude-x-2"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(first_free("claude-x", &taken), "claude-x-3");
}

#[test]
fn a_gap_is_reused_before_counting_up() {
    // An agent that left frees its number; the next one takes it rather than
    // the names climbing forever over a long day.
    let taken: HashSet<String> = ["claude-x-1", "claude-x-3"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(first_free("claude-x", &taken), "claude-x-2");
}

#[test]
fn another_prefix_does_not_count_as_taken() {
    let taken: HashSet<String> = ["codex-x-1"].iter().map(|s| s.to_string()).collect();
    assert_eq!(first_free("claude-x", &taken), "claude-x-1");
}

// ---- lifecycle -------------------------------------------------------------

#[test]
fn registering_puts_the_agent_on_the_bus_and_leaving_takes_it_off() {
    with_test_db(|| {
        let mut p = Presence::register(registration("t-arrive")).unwrap();
        assert!(on_bus("t-arrive"));
        assert_eq!(p.name(), "t-arrive");
        // Checked on the bus, not on the struct: what matters is what the other
        // agents see, and that is the registry row.
        let row = broker::list_agents(None)
            .unwrap()
            .into_iter()
            .find(|a| a.id.name == "t-arrive")
            .expect("registered agent missing from the bus");
        assert_eq!(row.id.nick.as_deref(), Some("otter"));
        assert_eq!(row.id.session.as_deref(), Some("/tmp/proj"));
        assert_eq!(row.id.agent_type.as_deref(), Some("sidekar"));
        p.leave();
        assert!(!on_bus("t-arrive"));
    });
}

#[test]
fn dropping_a_presence_leaves_the_bus() {
    // The case this module exists for: a `?` or `bail!` between registering and
    // the end of the session. Before, REPL returned on a missing model with the
    // registration still in place, and the dead process sat in `bus who`.
    with_test_db(|| {
        {
            let _p = Presence::register(registration("t-dropped")).unwrap();
            assert!(on_bus("t-dropped"));
        }
        assert!(
            !on_bus("t-dropped"),
            "a dropped presence left a ghost on the bus"
        );
    });
}

#[test]
fn an_early_return_does_not_strand_a_registration() {
    // The REPL bug, shaped the way it actually happened.
    fn run() -> Result<()> {
        let _p = Presence::register(registration("t-early"))?;
        anyhow::bail!("Single-prompt mode requires -c <credential>");
    }
    with_test_db(|| {
        assert!(run().is_err());
        assert!(!on_bus("t-early"));
    });
}

#[test]
fn leaving_twice_is_harmless() {
    // PTY calls leave() before process::exit, and the value may still be
    // dropped on other paths; the second departure must be a no-op rather than
    // unregistering a newer agent that has since taken the same name.
    with_test_db(|| {
        let mut p = Presence::register(registration("t-twice")).unwrap();
        p.leave();
        let _reuser = Presence::register(registration("t-twice")).unwrap();
        p.leave();
        drop(p);
        assert!(
            on_bus("t-twice"),
            "a stale leave() removed the agent that reused the name"
        );
    });
}

#[test]
fn a_history_row_is_opened_on_arrival_and_closed_on_departure() {
    with_test_db(|| {
        let mut r = registration("t-history");
        r.history = Some(History {
            id: "pty:1:100".into(),
            agent: "claude".into(),
            cwd: "/tmp/proj".into(),
            started_at: 100,
        });
        let mut p = Presence::register(r).unwrap();
        let row = broker::get_agent_session("pty:1:100")
            .unwrap()
            .expect("history row missing");
        assert!(row.ended_at.is_none(), "row closed before the agent left");
        p.leave();
        let row = broker::get_agent_session("pty:1:100")
            .unwrap()
            .expect("history row vanished");
        assert!(
            row.ended_at.is_some(),
            "leaving did not close the history row"
        );
    });
}

#[test]
fn no_history_row_is_written_unless_asked_for() {
    // REPL and CLI record none today. Keeping it that way is deliberate for now:
    // this module was meant to change no behaviour, and adding REPL history is
    // a decision rather than a refactor.
    with_test_db(|| {
        let _p = Presence::register(registration("t-nohistory")).unwrap();
        let all = broker::list_agent_sessions(false, None, 100).unwrap_or_default();
        assert!(all.iter().all(|s| s.agent_name != "t-nohistory"));
    });
}

// ---- pane -> process -------------------------------------------------------

#[test]
fn every_pane_format_maps_back_to_its_pid() {
    assert_eq!(pid_of_pane("pty-4242"), Some(4242));
    assert_eq!(pid_of_pane("repl-17"), Some(17));
    assert_eq!(pid_of_pane("cli-9"), Some(9));
    assert_eq!(pid_of_pane("session-55"), Some(55));
    assert_eq!(pid_of_pane("mcp-12"), Some(12));
}

#[test]
fn a_pane_that_is_not_ours_has_no_pid() {
    assert_eq!(pid_of_pane("tmux-12"), None);
    assert_eq!(pid_of_pane("pty-"), None);
    assert_eq!(pid_of_pane("pty-abc"), None);
    assert_eq!(pid_of_pane(""), None);
    // A bare number is not a pane; guessing it was one could signal a stranger.
    assert_eq!(pid_of_pane("4242"), None);
}

#[test]
fn a_pane_can_never_yield_a_pid_that_means_everything() {
    // `stop` passes this to kill(pid, SIGTERM). 0 is "my process group" and
    // -1 is "every process I may signal"; either would make one malformed
    // registry row a kill-everything.
    assert_eq!(pid_of_pane("pty-0"), None);
    assert_eq!(pid_of_pane("pty--1"), None);
    assert_eq!(pid_of_pane("repl--4242"), None);
}

#[test]
fn this_process_is_alive_and_nonsense_pids_are_not() {
    assert!(process_alive(std::process::id() as i32));
    assert!(
        !process_alive(i32::MAX - 1),
        "a pid that cannot exist read as alive"
    );
    assert!(!process_alive(0));
    assert!(!process_alive(-1));
}

// ---- leaving settles mail ----------------------------------------------------

#[test]
fn leaving_withdraws_undelivered_mail_and_tells_the_sender() {
    // The wiring, not the function: bounce_mail_for_departed is tested in the
    // broker; this checks leave() actually calls it, and before the name is
    // freed — so the next holder of the name cannot inherit the mail.
    with_test_db(|| {
        let _sender = Presence::register(registration("t-sender")).unwrap();
        let mut leaver = Presence::register(registration("t-leaver")).unwrap();
        broker::enqueue_bus_message("t-leaver", "t-sender", "are you there", true, None).unwrap();

        leaver.leave();

        let for_leaver = broker::list_queued_messages("t-leaver").unwrap();
        assert!(for_leaver.is_empty(), "mail stayed queued on a freed name");
        let for_sender = broker::list_queued_messages("t-sender").unwrap();
        assert_eq!(for_sender.len(), 1, "the sender was not told");
        assert!(
            for_sender[0].body.contains("are you there"),
            "{}",
            for_sender[0].body
        );

        // A different agent takes the name and must find nothing waiting.
        let _next = Presence::register(registration("t-leaver")).unwrap();
        assert!(broker::list_queued_messages("t-leaver").unwrap().is_empty());
    });
}
