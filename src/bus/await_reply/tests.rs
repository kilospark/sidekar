use super::*;
use crate::message::AgentId;

/// Run `f` against a throwaway broker database. Everything here reaches the
/// broker through `open()`, which re-reads HOME on each call.
fn with_test_db(f: impl FnOnce()) {
    // Restores HOME and removes the directory even if `f` panics.
    let _home = crate::ScratchHome::new();
    f();
}

fn block_on<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(f)
}

fn agent(name: &str, pane: &str) -> AgentId {
    AgentId {
        name: name.into(),
        nick: Some(name.into()),
        session: Some("/tmp/proj".into()),
        pane: Some(pane.into()),
        agent_type: Some("sidekar".into()),
    }
}

fn register(name: &str, pane: &str) -> AgentId {
    let id = agent(name, pane);
    broker::register_agent(&id, Some(pane)).unwrap();
    id
}

/// `asker` sends a tracked request to `to`, as `bus send` or `spawn` does.
fn request(asker: &AgentId, to: &str) -> Envelope {
    let env = Envelope::new_request(asker.clone(), to, "review the diff");
    broker::set_pending(&env).unwrap();
    broker::set_outbound_request(&env, "asker", "broker", to, None, None).unwrap();
    env
}

/// `from` answers `req`, including the copy queued for the asker's pane.
fn answer(from: &AgentId, req: &Envelope, text: &str) -> Envelope {
    let reply = Envelope::new_response(from.clone(), &req.from.name, text, req.id.clone());
    broker::enqueue_bus_message(&req.from.name, &from.name, text, true, Some(&reply)).unwrap();
    broker::record_reply(&req.id, &reply).unwrap();
    reply
}

const SHORT: Duration = Duration::from_millis(600);

// ---- outcomes --------------------------------------------------------------

#[test]
fn an_answer_is_returned() {
    with_test_db(|| {
        let asker = register("asker", "cli-1");
        let helper = register("helper", "pty-2");
        let req = request(&asker, "helper");
        answer(&helper, &req, "looks fine");
        match block_on(await_reply(&req.id, None, SHORT)).unwrap() {
            AwaitOutcome::Answered(r) => assert_eq!(r.message, "looks fine"),
            other => panic!("expected an answer, got {other:?}"),
        }
    });
}

#[test]
fn a_recipient_that_left_without_answering_ends_the_wait_at_once() {
    with_test_db(|| {
        let asker = register("asker", "cli-1");
        register("helper", "pty-2");
        let req = request(&asker, "helper");
        broker::bounce_mail_for_departed("helper", "helper", crate::message::epoch_secs()).unwrap();
        broker::unregister_agent("helper").unwrap();
        let started = Instant::now();
        let outcome = block_on(await_reply(&req.id, None, Duration::from_secs(30))).unwrap();
        assert!(
            matches!(outcome, AwaitOutcome::RecipientGone { ref recipient } if recipient == "helper")
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "should not wait out the timeout"
        );
    });
}

#[test]
fn a_crashed_recipient_is_noticed_without_its_request_being_closed() {
    // Nothing closed the request — the agent died without leaving — so only
    // watching the recipient can tell.
    with_test_db(|| {
        let asker = register("asker", "cli-1");
        register("helper", "pty-2");
        let req = request(&asker, "helper");
        let recipient = Recipient::of_request(&req.id);
        broker::unregister_agent("helper").unwrap();
        let outcome = block_on(await_reply(&req.id, recipient, Duration::from_secs(30))).unwrap();
        assert!(matches!(outcome, AwaitOutcome::RecipientGone { .. }));
    });
}

#[test]
fn a_new_agent_under_the_same_name_is_not_mistaken_for_the_recipient() {
    with_test_db(|| {
        let asker = register("asker", "cli-1");
        register("helper", "pty-2");
        let req = request(&asker, "helper");
        let recipient = Recipient::of_request(&req.id);
        broker::unregister_agent("helper").unwrap();
        register("helper", "pty-3");
        let outcome = block_on(await_reply(&req.id, recipient, Duration::from_secs(30))).unwrap();
        assert!(matches!(outcome, AwaitOutcome::RecipientGone { .. }));
    });
}

#[test]
fn an_answer_sent_just_before_leaving_still_counts() {
    with_test_db(|| {
        let asker = register("asker", "cli-1");
        let helper = register("helper", "pty-2");
        let req = request(&asker, "helper");
        let recipient = Recipient::of_request(&req.id);
        answer(&helper, &req, "done");
        broker::unregister_agent("helper").unwrap();
        let outcome = block_on(await_reply(&req.id, recipient, SHORT)).unwrap();
        assert!(matches!(outcome, AwaitOutcome::Answered(_)));
    });
}

#[test]
fn a_cancelled_request_ends_the_wait() {
    with_test_db(|| {
        let asker = register("asker", "cli-1");
        register("helper", "pty-2");
        let req = request(&asker, "helper");
        broker::cancel_outbound_request(&req.id, crate::message::epoch_secs()).unwrap();
        let outcome = block_on(await_reply(&req.id, None, Duration::from_secs(30))).unwrap();
        assert!(matches!(outcome, AwaitOutcome::Cancelled));
    });
}

#[test]
fn the_buses_five_minute_warning_does_not_end_the_wait() {
    // `timed_out` only warns the sender; the answer can still come.
    with_test_db(|| {
        let asker = register("asker", "cli-1");
        register("helper", "pty-2");
        let req = request(&asker, "helper");
        broker::mark_outbound_timed_out(&req.id, crate::message::epoch_secs()).unwrap();
        let outcome = block_on(await_reply(&req.id, None, SHORT)).unwrap();
        assert!(matches!(outcome, AwaitOutcome::TimedOut));
    });
}

#[test]
fn silence_times_out() {
    with_test_db(|| {
        let asker = register("asker", "cli-1");
        register("helper", "pty-2");
        let req = request(&asker, "helper");
        let started = Instant::now();
        let outcome = block_on(await_reply(&req.id, None, SHORT)).unwrap();
        assert!(matches!(outcome, AwaitOutcome::TimedOut));
        assert!(started.elapsed() >= SHORT);
    });
}

// ---- after the answer ------------------------------------------------------

#[test]
fn a_consumed_answer_is_not_pasted_again() {
    with_test_db(|| {
        let asker = register("asker", "cli-1");
        let helper = register("helper", "pty-2");
        let req = request(&asker, "helper");
        answer(&helper, &req, "looks fine");
        assert_eq!(broker::list_queued_messages("asker").unwrap().len(), 1);
        let text = answer_or_exit(
            &req.id,
            block_on(await_reply(&req.id, None, SHORT)).unwrap(),
            SHORT,
        )
        .unwrap();
        assert_eq!(text, "looks fine");
        assert!(broker::list_queued_messages("asker").unwrap().is_empty());
    });
}

#[test]
fn an_answer_that_beat_the_request_row_still_closes_it() {
    // A fast delegate can answer before the asker records the request. The
    // answer is found by id regardless; the request must then not be left
    // open, or its recipient is reminded to answer what it already answered.
    with_test_db(|| {
        let asker = register("asker", "cli-1");
        let helper = register("helper", "pty-2");
        let req = Envelope::new_request(asker.clone(), "helper", "review the diff");
        answer(&helper, &req, "done");
        broker::set_outbound_request(&req, "asker", "broker", "helper", None, None).unwrap();
        let outcome = block_on(await_reply(&req.id, None, SHORT)).unwrap();
        answer_or_exit(&req.id, outcome, SHORT).unwrap();
        let status = broker::outbound_request(&req.id).unwrap().unwrap().status;
        assert_eq!(status, broker::OUTBOUND_STATUS_ANSWERED);
    });
}

#[test]
fn no_answer_ever_exits_differently_from_no_answer_yet() {
    // In a test database: reporting a departure settles it in the broker.
    with_test_db(|| {
        let gone = answer_or_exit(
            "m1",
            AwaitOutcome::RecipientGone {
                recipient: "helper".into(),
            },
            SHORT,
        )
        .unwrap_err();
        assert_eq!(
            gone.downcast_ref::<ExitWith>().map(|e| e.code),
            Some(EXIT_NO_ANSWER)
        );
        let cancelled = answer_or_exit("m1", AwaitOutcome::Cancelled, SHORT).unwrap_err();
        assert_eq!(
            cancelled.downcast_ref::<ExitWith>().map(|e| e.code),
            Some(EXIT_NO_ANSWER)
        );
        let timed_out = answer_or_exit("m1", AwaitOutcome::TimedOut, SHORT).unwrap_err();
        assert!(timed_out.downcast_ref::<ExitWith>().is_none());
    });
}

// ---- durations -------------------------------------------------------------

#[test]
fn durations_take_units_or_bare_seconds() {
    assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
    assert_eq!(parse_duration("10m").unwrap(), Duration::from_secs(600));
    assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
    assert_eq!(parse_duration("45").unwrap(), Duration::from_secs(45));
    assert!(parse_duration("0").is_err());
    assert!(parse_duration("5x").is_err());
    assert!(parse_duration("m").is_err());
    assert!(parse_duration("").is_err());
}

#[test]
fn durations_are_described_in_their_largest_whole_unit() {
    assert_eq!(describe(Duration::from_secs(600)), "10m");
    assert_eq!(describe(Duration::from_secs(7200)), "2h");
    assert_eq!(describe(Duration::from_secs(90)), "90s");
}

// ---- the departure notice --------------------------------------------------

fn queued_notices(to: &str) -> Vec<String> {
    broker::list_queued_messages(to)
        .unwrap()
        .into_iter()
        .filter(|m| m.sender == "sidekar")
        .map(|m| m.body)
        .collect()
}

#[test]
fn a_departure_the_wait_reported_is_not_pasted_again() {
    with_test_db(|| {
        let asker = register("asker", "cli-1");
        register("helper", "pty-2");
        let req = request(&asker, "helper");
        broker::bounce_mail_for_departed("helper", "helper", crate::message::epoch_secs()).unwrap();
        broker::unregister_agent("helper").unwrap();
        assert_eq!(queued_notices("asker").len(), 1);
        let outcome = block_on(await_reply(&req.id, None, SHORT)).unwrap();
        assert!(answer_or_exit(&req.id, outcome, SHORT).is_err());
        assert!(queued_notices("asker").is_empty());
    });
}

#[test]
fn a_notice_about_more_than_the_request_keeps_the_rest() {
    with_test_db(|| {
        let asker = register("asker", "cli-1");
        register("helper", "pty-2");
        let req = request(&asker, "helper");
        broker::enqueue_bus_message("helper", "asker", "also: the build is red", true, None)
            .unwrap();
        broker::bounce_mail_for_departed("helper", "helper", crate::message::epoch_secs()).unwrap();
        broker::unregister_agent("helper").unwrap();
        let outcome = block_on(await_reply(&req.id, None, SHORT)).unwrap();
        assert!(answer_or_exit(&req.id, outcome, SHORT).is_err());
        let left = queued_notices("asker");
        assert_eq!(left.len(), 1);
        assert!(left[0].contains("the build is red"));
        assert!(!left[0].contains(&req.id));
    });
}

#[test]
fn a_crash_the_wait_reported_is_not_announced_later_by_the_sweep() {
    // The recipient died without closing its request. The wait noticed first;
    // the daemon's sweep must then find nothing left to announce.
    with_test_db(|| {
        let asker = register("asker", "cli-1");
        register("helper", "pty-2");
        let req = request(&asker, "helper");
        let recipient = Recipient::of_request(&req.id);
        broker::unregister_agent("helper").unwrap();
        let outcome = block_on(await_reply(&req.id, recipient, SHORT)).unwrap();
        assert!(answer_or_exit(&req.id, outcome, SHORT).is_err());
        assert!(
            broker::orphaned_mail_recipients(crate::message::epoch_secs() + 3600, 0)
                .unwrap()
                .is_empty()
        );
        assert!(queued_notices("asker").is_empty());
    });
}

#[test]
fn removing_a_notices_only_line_leaves_nothing_to_send() {
    let one = "[sidekar] x left:\n  - request m1 went unanswered: hi";
    assert_eq!(
        broker::without_line(one, "request m1 went unanswered:"),
        None
    );
    let two =
        "[sidekar] x left:\n  - request m1 went unanswered: hi\n  - message not delivered: yo";
    assert_eq!(
        broker::without_line(two, "request m1 went unanswered:").as_deref(),
        Some("[sidekar] x left:\n  - message not delivered: yo")
    );
}

#[test]
fn whoever_stops_an_agent_is_not_told_it_left() {
    with_test_db(|| {
        let me = register("me", "cli-1");
        let other = register("other", "cli-3");
        register("helper", "pty-2");
        let mine = request(&me, "helper");
        let theirs = request(&other, "helper");
        assert_eq!(
            broker::cancel_requests_before_stopping("me", "helper").unwrap(),
            1
        );
        broker::bounce_mail_for_departed("helper", "helper", crate::message::epoch_secs()).unwrap();
        assert!(queued_notices("me").is_empty());
        let told = queued_notices("other");
        assert_eq!(told.len(), 1);
        assert!(told[0].contains(&theirs.id));
        let status = broker::outbound_request(&mine.id).unwrap().unwrap().status;
        assert_eq!(status, broker::OUTBOUND_STATUS_CANCELLED);
    });
}
