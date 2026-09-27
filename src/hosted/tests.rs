use super::cli::{TurnWatch, Waited};
use super::protocol::{Event, EventBody};
use super::*;
use serde_json::json;

fn meta(name: &str, pid: i32, status: Status) -> Meta {
    Meta {
        name: name.into(),
        engine: "claude".into(),
        cwd: "/tmp".into(),
        model: None,
        approvals: ApprovalPolicy::Ask,
        status,
        pid,
        engine_pid: 0,
        engine_session_id: None,
        created_at: 1,
        ended_at: None,
        exit_code: None,
    }
}

/// A pid that belonged to a process and no longer does.
fn dead_pid() -> i32 {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id() as i32;
    child.wait().unwrap();
    pid
}

#[test]
fn a_dead_host_is_marked_ended_and_its_socket_removed() {
    let _home = crate::ScratchHome::new();
    ensure_private_dir(&dir_of("claude-1")).unwrap();
    let mut m = meta("claude-1", dead_pid(), Status::Running);
    write_meta(&m).unwrap();
    std::fs::write(socket_path("claude-1"), b"").unwrap();
    assert!(reap(&mut m).unwrap());
    assert_eq!(read_meta("claude-1").unwrap().status, Status::Ended);
    assert!(!socket_path("claude-1").exists());
    assert!(!reap(&mut m).unwrap(), "a second reap changes nothing");
}

#[test]
fn a_live_host_is_left_alone() {
    let _home = crate::ScratchHome::new();
    ensure_private_dir(&dir_of("claude-1")).unwrap();
    let mut m = meta("claude-1", std::process::id() as i32, Status::Running);
    write_meta(&m).unwrap();
    assert!(!reap(&mut m).unwrap());
    assert_eq!(read_meta("claude-1").unwrap().status, Status::Running);
}

#[test]
fn names_skip_live_sessions_and_reuse_ended_ones() {
    let _home = crate::ScratchHome::new();
    for (name, pid, status) in [
        ("claude-1", std::process::id() as i32, Status::Running),
        ("claude-2", dead_pid(), Status::Ended),
    ] {
        ensure_private_dir(&dir_of(name)).unwrap();
        write_meta(&meta(name, pid, status)).unwrap();
    }
    assert_eq!(free_name("claude"), "claude-2");
}

#[test]
fn reaping_takes_a_dead_session_off_the_bus() {
    let _home = crate::ScratchHome::new();
    let pid = dead_pid();
    let pane = format!("session-{pid}");
    let id = crate::message::AgentId {
        name: "claude-1".into(),
        nick: Some("claude-1".into()),
        session: Some("/tmp".into()),
        pane: Some(pane.clone()),
        agent_type: Some("session".into()),
    };
    crate::broker::register_agent(&id, Some(&pane)).unwrap();
    ensure_private_dir(&dir_of("claude-1")).unwrap();
    let mut m = meta("claude-1", pid, Status::Running);
    write_meta(&m).unwrap();
    reap(&mut m).unwrap();
    assert!(!crate::broker::agent_is_registered("claude-1").unwrap());
}

#[test]
fn an_old_ended_session_is_deleted_and_a_recent_one_kept() {
    let _home = crate::ScratchHome::new();
    let now = crate::message::epoch_secs();
    for (name, ended) in [
        ("claude-1", now - RETENTION_SECS - 60),
        ("claude-2", now - 60),
    ] {
        ensure_private_dir(&dir_of(name)).unwrap();
        let mut m = meta(name, dead_pid(), Status::Ended);
        m.ended_at = Some(ended);
        write_meta(&m).unwrap();
    }
    reap_all();
    assert!(!dir_of("claude-1").exists());
    assert!(dir_of("claude-2").exists());
}

#[test]
fn only_an_engine_command_line_counts_as_the_engine() {
    assert!(is_engine_command(
        "/opt/homebrew/bin/claude -p --input-format stream-json --output-format stream-json",
        "claude"
    ));
    assert!(
        !is_engine_command("claude --resume abc", "claude"),
        "an interactive claude"
    );
    assert!(!is_engine_command("vim claude-stream-json.md", "claude"));
    assert!(!is_engine_command("", "claude"));
}

// ---- waiting on a turn -----------------------------------------------------

fn ev(seq: u64, body: EventBody) -> Event {
    Event {
        seq,
        ts: 0,
        body,
        raw: None,
    }
}

fn asked(seq: u64, turn: &str, id: &str) -> Event {
    ev(
        seq,
        EventBody::ApprovalRequested {
            turn_id: turn.into(),
            request_id: id.into(),
            tool: "Bash".into(),
            input: json!({}),
        },
    )
}

fn resolved(seq: u64, id: &str) -> Event {
    ev(
        seq,
        EventBody::ApprovalResolved {
            request_id: id.into(),
            allow: true,
            by: "client".into(),
        },
    )
}

fn done(seq: u64, turn: &str) -> Event {
    ev(
        seq,
        EventBody::TurnDone {
            turn_id: turn.into(),
            result: format!("{turn} result"),
            is_error: false,
            reason: None,
        },
    )
}

#[test]
fn the_wait_ends_on_its_own_turn_only() {
    let mut w = TurnWatch::default();
    assert_eq!(
        w.observe(&done(1, "t1"), "t2", ApprovalPolicy::Ask, true),
        None
    );
    assert_eq!(
        w.observe(&done(2, "t2"), "t2", ApprovalPolicy::Ask, true),
        Some(Waited::Done {
            result: "t2 result".into(),
            is_error: false
        })
    );
}

#[test]
fn a_new_approval_under_ask_stops_the_wait() {
    let mut w = TurnWatch::default();
    let out = w.observe(&asked(1, "t1", "r1"), "t1", ApprovalPolicy::Ask, true);
    assert!(matches!(out, Some(Waited::NeedsApproval(ref r)) if r["request_id"] == "r1"));
}

#[test]
fn under_allow_or_deny_an_approval_is_not_the_callers_business() {
    let mut w = TurnWatch::default();
    for policy in [ApprovalPolicy::Allow, ApprovalPolicy::Deny] {
        assert_eq!(w.observe(&asked(1, "t1", "r1"), "t1", policy, true), None);
    }
}

#[test]
fn an_approval_answered_in_the_past_is_not_news() {
    let mut w = TurnWatch::default();
    assert_eq!(
        w.observe(&asked(1, "t1", "r1"), "t1", ApprovalPolicy::Ask, false),
        None
    );
    assert_eq!(
        w.observe(&resolved(2, "r1"), "t1", ApprovalPolicy::Ask, false),
        None
    );
    assert_eq!(w.unresolved(), None);
}

#[test]
fn an_approval_still_open_after_the_replay_is() {
    let mut w = TurnWatch::default();
    w.observe(&asked(1, "t1", "r1"), "t1", ApprovalPolicy::Ask, false);
    assert!(matches!(w.unresolved(), Some(Waited::NeedsApproval(_))));
}

#[test]
fn the_session_ending_ends_the_wait() {
    let mut w = TurnWatch::default();
    let end = ev(1, EventBody::SessionEnded { exit_code: Some(0) });
    assert_eq!(
        w.observe(&end, "t1", ApprovalPolicy::Ask, true),
        Some(Waited::Ended)
    );
}

#[test]
fn events_round_trip_with_their_type_tag() {
    let e = done(7, "t3");
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v["type"], "turn_done");
    assert_eq!(v["seq"], 7);
    assert_eq!(serde_json::from_value::<Event>(v).unwrap(), e);
}
