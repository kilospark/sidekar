//! The host's bookkeeping, driven with the real Claude adapter and lines in
//! the shapes Claude emits — no process.

use super::*;
use serde_json::json;

fn state(policy: ApprovalPolicy) -> State {
    State::new(
        Box::new(crate::hosted::claude::Claude::default()),
        policy,
        false,
    )
}

fn result(text: &str) -> Value {
    json!({"type": "result", "subtype": "success", "is_error": false, "result": text,
           "terminal_reason": "completed"})
}

fn approval(id: &str) -> Value {
    json!({"type": "control_request", "request_id": id, "request": {
        "subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "touch x"}}})
}

fn types(s: &State) -> Vec<String> {
    s.events()
        .iter()
        .map(|e| {
            serde_json::to_value(e).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

fn outbox_json(s: &mut State) -> Vec<Value> {
    s.take_outbox()
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn a_send_starts_a_turn_and_writes_the_message() {
    let mut s = state(ApprovalPolicy::Ask);
    let reply = s.send("hi".into(), Busy::Reject);
    assert_eq!(reply.turn_id.as_deref(), Some("t1"));
    assert_eq!(outbox_json(&mut s)[0]["type"], "user");
    s.on_engine_line(result("hello"));
    let events = s.events();
    let done = events.last().unwrap();
    assert_eq!(
        done.body,
        EventBody::TurnDone {
            turn_id: "t1".into(),
            result: "hello".into(),
            is_error: false,
            reason: Some("completed".into())
        }
    );
    assert!(done.raw.is_some(), "the engine's line is kept");
}

#[test]
fn a_send_during_a_turn_is_refused_by_default() {
    let mut s = state(ApprovalPolicy::Ask);
    s.send("one".into(), Busy::Reject);
    let reply = s.send("two".into(), Busy::Reject);
    assert!(!reply.ok);
    assert_eq!(reply.code.as_deref(), Some("busy"));
    // A refused send does not use up a turn number.
    s.on_engine_line(result("r1"));
    assert_eq!(
        s.send("three".into(), Busy::Reject).turn_id.as_deref(),
        Some("t2")
    );
}

#[test]
fn a_queued_send_runs_when_the_turn_ends() {
    let mut s = state(ApprovalPolicy::Ask);
    s.send("one".into(), Busy::Reject);
    s.take_outbox();
    assert_eq!(
        s.send("two".into(), Busy::Queue).turn_id.as_deref(),
        Some("t2")
    );
    assert!(
        s.take_outbox().is_empty(),
        "nothing is written while t1 runs"
    );
    s.on_engine_line(result("r1"));
    let out = outbox_json(&mut s);
    assert_eq!(out[0]["message"]["content"], "two");
    assert_eq!(s.status()["current"], "t2");
}

#[test]
fn an_interrupting_send_stops_the_turn_then_runs() {
    let mut s = state(ApprovalPolicy::Ask);
    s.send("long".into(), Busy::Reject);
    s.take_outbox();
    s.send("instead".into(), Busy::Interrupt);
    assert_eq!(outbox_json(&mut s)[0]["request"]["subtype"], "interrupt");
    s.on_engine_line(json!({"type": "result", "is_error": true,
        "terminal_reason": "aborted_streaming", "errors": ["[ede_diagnostic]"]}));
    assert_eq!(outbox_json(&mut s)[0]["message"]["content"], "instead");
}

#[test]
fn a_turn_the_engine_starts_gets_its_own_id() {
    let mut s = state(ApprovalPolicy::Ask);
    s.on_engine_line(json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "The build finished."}]}}));
    s.on_engine_line(
        json!({"type": "result", "is_error": false, "result": "The build finished.",
        "origin": {"kind": "task-notification"}}),
    );
    assert_eq!(types(&s), ["turn_started", "text", "turn_done"]);
    assert_eq!(
        s.events()[0].body,
        EventBody::TurnStarted {
            turn_id: "e1".into(),
            source: "engine".into(),
            text: None
        }
    );
    // Nothing of ours was running, and nothing is now.
    assert_eq!(s.status()["current"], Value::Null);
}

#[test]
fn an_engine_turn_ending_does_not_end_ours() {
    let mut s = state(ApprovalPolicy::Ask);
    s.send("mine".into(), Busy::Reject);
    s.on_engine_line(
        json!({"type": "result", "is_error": false, "result": "background done",
        "origin": {"kind": "task-notification"}}),
    );
    assert_eq!(s.status()["current"], "t1");
    s.on_engine_line(result("my answer"));
    assert_eq!(s.status()["current"], Value::Null);
}

#[test]
fn the_conversation_id_is_reported_once() {
    let mut s = state(ApprovalPolicy::Ask);
    let init = json!({"type": "system", "subtype": "init", "session_id": "cc82", "model": "m"});
    s.on_engine_line(init.clone());
    s.on_engine_line(init);
    assert_eq!(types(&s), ["engine_ready"]);
    assert_eq!(s.status()["engine_session_id"], "cc82");
}

#[test]
fn allow_and_deny_policies_answer_without_asking() {
    for (policy, allow) in [(ApprovalPolicy::Allow, true), (ApprovalPolicy::Deny, false)] {
        let mut s = state(policy);
        s.send("go".into(), Busy::Reject);
        s.take_outbox();
        s.on_engine_line(approval("r1"));
        let out = outbox_json(&mut s);
        let behavior = &out[0]["response"]["response"]["behavior"];
        assert_eq!(behavior, if allow { "allow" } else { "deny" });
        assert_eq!(
            s.events().last().unwrap().body,
            EventBody::ApprovalResolved {
                request_id: "r1".into(),
                allow,
                by: "policy".into()
            }
        );
    }
}

#[test]
fn ask_waits_for_an_answer() {
    let mut s = state(ApprovalPolicy::Ask);
    s.send("go".into(), Busy::Reject);
    s.take_outbox();
    s.on_engine_line(approval("r1"));
    assert!(
        s.take_outbox().is_empty(),
        "nothing is answered on the caller's behalf"
    );
    assert!(!s.approve("nope", true, None).ok);
    assert!(s.approve("r1", true, None).ok);
    assert_eq!(
        outbox_json(&mut s)[0]["response"]["response"]["behavior"],
        "allow"
    );
    assert!(!s.approve("r1", true, None).ok, "answered once only");
}

#[test]
fn an_unanswered_approval_is_denied_on_timeout() {
    let mut s = state(ApprovalPolicy::Ask);
    s.send("go".into(), Busy::Reject);
    s.take_outbox();
    s.on_engine_line(approval("r1"));
    s.expire_approvals(Duration::ZERO);
    assert_eq!(
        outbox_json(&mut s)[0]["response"]["response"]["behavior"],
        "deny"
    );
    assert_eq!(
        s.events().last().unwrap().body,
        EventBody::ApprovalResolved {
            request_id: "r1".into(),
            allow: false,
            by: "timeout".into()
        }
    );
}

#[test]
fn approvals_of_a_finished_turn_are_dropped() {
    let mut s = state(ApprovalPolicy::Ask);
    s.send("go".into(), Busy::Reject);
    s.on_engine_line(approval("r1"));
    s.on_engine_line(result("gave up"));
    assert_eq!(s.status()["pending_approvals"], json!([]));
}

#[test]
fn cancel_with_nothing_running_says_so() {
    let mut s = state(ApprovalPolicy::Ask);
    assert_eq!(s.cancel().code.as_deref(), Some("idle"));
}

#[test]
fn a_bus_request_becomes_a_turn_and_its_result_the_answer() {
    let mut s = state(ApprovalPolicy::Deny);
    let from = crate::message::AgentId::new("asker");
    let request = crate::message::Envelope::new_request(from, "claude-1", "What is 5*5?");
    s.send_from_bus(request.clone());
    assert!(matches!(
        &s.events()[0].body,
        EventBody::TurnStarted { source, .. } if source == "bus"
    ));
    let written = outbox_json(&mut s);
    let text = written[0]["message"]["content"].as_str().unwrap();
    assert!(text.contains("What is 5*5?"));
    assert!(text.contains("sent back to them automatically"));
    s.on_engine_line(result("25"));
    let answers = s.take_bus_answers();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].0.id, request.id);
    assert_eq!(answers[0].1, "25");
}

#[test]
fn a_bus_note_says_no_reply_is_needed() {
    let note =
        crate::message::Envelope::new_fyi(crate::message::AgentId::new("a"), "b", "FYI: deployed");
    assert!(bus_turn_text(&note).contains("No reply is needed"));
}

#[test]
fn a_late_client_still_gets_events_memory_has_let_go_of() {
    let log = std::env::temp_dir().join(format!(
        "sidekar-host-log-{}-{}.jsonl",
        std::process::id(),
        crate::message::epoch_secs()
    ));
    let mut s = state(ApprovalPolicy::Ask);
    s.sink = Some(std::fs::File::create(&log).unwrap());
    s.log = Some(log.clone());
    for i in 0..(EVENTS_IN_MEMORY + 50) {
        s.emit(EventBody::EngineEvent {}, Some(json!({"n": i})));
    }
    assert_eq!(s.events().len(), EVENTS_IN_MEMORY, "memory is capped");
    let all = s.backlog(0);
    assert_eq!(all.len(), EVENTS_IN_MEMORY + 50);
    assert!(
        all.windows(2).all(|w| w[1].seq == w[0].seq + 1),
        "in order, no gaps"
    );
    assert_eq!(s.backlog(s.seq() - 3).len(), 3);
    let _ = std::fs::remove_file(&log);
}

fn state_with_refresh(policy: ApprovalPolicy) -> State {
    State::new(
        Box::new(crate::hosted::claude::Claude::default()),
        policy,
        true,
    )
}

fn proxy_env(value: &str) -> HashMap<String, String> {
    [("https_proxy".to_string(), value.to_string())]
        .into_iter()
        .collect()
}

fn send_with_env(s: &mut State, text: &str, busy: Busy, proxy: &str) -> Reply {
    s.handle(Request::Send {
        text: text.into(),
        busy,
        follow: false,
        env: Some(proxy_env(proxy)),
    })
    .0
}

#[test]
fn a_new_env_on_an_idle_session_restarts_before_the_turn_is_written() {
    let mut s = state_with_refresh(ApprovalPolicy::Ask);
    s.engine_env = proxy_env("http://old:3128");
    assert!(send_with_env(&mut s, "hi", Busy::Reject, "http://new:3128").ok);
    let (at, env) = s.take_restart().expect("restart");
    assert_eq!(env, proxy_env("http://new:3128"));
    let outbox = s.take_outbox();
    assert_eq!(at, 0, "the new engine gets the whole turn");
    assert!(outbox[at].contains("\"hi\""));
    s.restarted(env.clone());
    assert_eq!(s.engine_env, env);
    assert!(matches!(
        s.events().last().unwrap().body,
        EventBody::EngineRestarted { .. }
    ));
}

#[test]
fn a_refused_send_changes_nothing() {
    let mut s = state_with_refresh(ApprovalPolicy::Ask);
    s.engine_env = proxy_env("http://old:3128");
    s.send("long task".into(), Busy::Reject);
    let reply = send_with_env(&mut s, "second", Busy::Reject, "http://new:3128");
    assert_eq!(reply.code.as_deref(), Some("busy"));
    s.on_engine_line(result("done"));
    s.send("third".into(), Busy::Reject);
    assert!(
        s.take_restart().is_none(),
        "the refused send's env was not adopted"
    );
}

#[test]
fn a_send_during_a_turn_defers_the_restart_to_the_next_turn() {
    // Restarting mid-turn killed the turn, and the session waited for its
    // result forever: every later send was refused as busy.
    let mut s = state_with_refresh(ApprovalPolicy::Ask);
    s.engine_env = proxy_env("http://old:3128");
    s.send("long task".into(), Busy::Reject);
    s.take_outbox();
    assert!(send_with_env(&mut s, "next", Busy::Queue, "http://new:3128").ok);
    assert!(s.take_restart().is_none(), "not while t1 runs");
    s.on_engine_line(result("t1 done"));
    let (at, env) = s.take_restart().expect("restart as t2 starts");
    assert_eq!(env, proxy_env("http://new:3128"));
    let outbox = s.take_outbox();
    assert!(outbox[at].contains("\"next\""), "t2 goes to the new engine");
    assert_eq!(s.status()["current"], "t2");
}

#[test]
fn an_interrupting_send_restarts_after_the_interrupt_reaches_the_old_engine() {
    let mut s = state_with_refresh(ApprovalPolicy::Ask);
    s.engine_env = proxy_env("http://old:3128");
    s.send("long task".into(), Busy::Reject);
    s.take_outbox();
    assert!(send_with_env(&mut s, "instead", Busy::Interrupt, "http://new:3128").ok);
    assert!(s.take_restart().is_none());
    assert!(
        s.take_outbox()[0].contains("interrupt"),
        "the old engine is told to stop"
    );
    s.on_engine_line(json!({"type": "result", "is_error": true,
        "terminal_reason": "aborted_streaming", "errors": []}));
    assert!(s.take_restart().is_some());
}

#[test]
fn a_turn_the_engine_started_defers_the_restart() {
    let mut s = state_with_refresh(ApprovalPolicy::Ask);
    s.engine_env = proxy_env("http://old:3128");
    s.on_engine_line(
        json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "bg"}]}}),
    );
    assert!(send_with_env(&mut s, "hi", Busy::Reject, "http://new:3128").ok);
    assert!(
        s.take_restart().is_none(),
        "e1 is still running on this engine"
    );
}

#[test]
fn the_same_env_as_the_engine_needs_no_restart() {
    let mut s = state_with_refresh(ApprovalPolicy::Ask);
    s.engine_env = proxy_env("http://same:3128");
    assert!(send_with_env(&mut s, "hi", Busy::Reject, "http://same:3128").ok);
    assert!(s.take_restart().is_none());
}

#[test]
fn without_refresh_env_a_callers_env_is_ignored() {
    let mut s = state(ApprovalPolicy::Ask);
    assert!(send_with_env(&mut s, "hi", Busy::Reject, "http://new:3128").ok);
    assert!(s.take_restart().is_none());
}

#[test]
fn only_proxy_variables_reach_the_engine() {
    let mut s = state_with_refresh(ApprovalPolicy::Ask);
    let mut env = proxy_env("http://new:3128");
    env.insert("DYLD_INSERT_LIBRARIES".into(), "/tmp/x.dylib".into());
    env.insert("NODE_OPTIONS".into(), "--require /tmp/x.js".into());
    s.handle(Request::Send {
        text: "hi".into(),
        busy: Busy::Reject,
        follow: false,
        env: Some(env),
    });
    let (_, applied) = s.take_restart().expect("restart");
    assert_eq!(applied, proxy_env("http://new:3128"));
}

#[test]
fn a_failed_restart_is_retried_at_the_next_turn() {
    let mut s = state_with_refresh(ApprovalPolicy::Ask);
    s.engine_env = proxy_env("http://old:3128");
    send_with_env(&mut s, "one", Busy::Reject, "http://new:3128");
    let (_, env) = s.take_restart().unwrap();
    s.restart_failed(env, "spawn failed".into());
    assert_eq!(
        s.engine_env,
        proxy_env("http://old:3128"),
        "still the old engine's"
    );
    s.on_engine_line(result("done on the old engine"));
    s.send("two".into(), Busy::Reject);
    assert!(s.take_restart().is_some(), "tried again");
}

#[test]
fn engine_command_resumes_the_conversation() {
    let mut s = state_with_refresh(ApprovalPolicy::Ask);
    s.engine_session_id = Some("sess-123".into());
    let (program, args) = s.engine_command(Some("sonnet".into()));
    assert_eq!(program, "claude");
    let resume_at = args.iter().position(|a| a == "--resume").unwrap();
    assert_eq!(args[resume_at + 1], "sess-123");
    let model_at = args.iter().position(|a| a == "--model").unwrap();
    assert_eq!(args[model_at + 1], "sonnet");
}
