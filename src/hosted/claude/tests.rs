//! Lines trimmed from real `claude -p --output-format stream-json` output.

use super::*;
use serde_json::json;

fn decode(line: Value) -> Vec<Output> {
    Claude::default().decode(&line)
}

#[test]
fn init_reports_the_conversation_id() {
    let out = decode(json!({
        "type": "system", "subtype": "init", "session_id": "cc82", "model": "claude-opus-5-5",
        "tools": ["Bash"], "cwd": "/tmp/x"
    }));
    assert_eq!(
        out,
        vec![Output::Ready {
            engine_session_id: "cc82".into(),
            model: Some("claude-opus-5-5".into())
        }]
    );
}

#[test]
fn assistant_text_and_tool_calls_become_their_own_outputs() {
    let out = decode(json!({"type": "assistant", "message": {"content": [
        {"type": "text", "text": "Running it."},
        {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "ls"}}
    ]}}));
    assert_eq!(
        out,
        vec![
            Output::Text("Running it.".into()),
            Output::ToolCall {
                id: "toolu_1".into(),
                name: "Bash".into(),
                input: json!({"command": "ls"})
            }
        ]
    );
}

#[test]
fn a_thinking_only_message_is_still_an_event() {
    let out = decode(json!({"type": "assistant", "message": {"content": [
        {"type": "thinking", "thinking": "…"}
    ]}}));
    assert_eq!(out, vec![Output::Other]);
}

#[test]
fn tool_results_read_string_and_list_content() {
    let out = decode(json!({"type": "user", "message": {"content": [
        {"type": "tool_result", "tool_use_id": "toolu_1", "content": "Denied by policy.", "is_error": true},
        {"type": "tool_result", "tool_use_id": "toolu_2", "content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]}
    ]}}));
    assert_eq!(
        out,
        vec![
            Output::ToolResult {
                id: "toolu_1".into(),
                is_error: true,
                content: "Denied by policy.".into()
            },
            Output::ToolResult {
                id: "toolu_2".into(),
                is_error: false,
                content: "a\nb".into()
            }
        ]
    );
}

#[test]
fn a_can_use_tool_request_is_an_approval() {
    let out = decode(
        json!({"type": "control_request", "request_id": "5e90", "request": {
            "subtype": "can_use_tool", "tool_name": "Bash",
            "input": {"command": "touch approved.txt"}, "permission_suggestions": []
        }}),
    );
    assert_eq!(
        out,
        vec![Output::ApprovalRequested {
            request_id: "5e90".into(),
            tool: "Bash".into(),
            input: json!({"command": "touch approved.txt"})
        }]
    );
}

#[test]
fn a_finished_turn_carries_its_result() {
    let out = decode(
        json!({"type": "result", "subtype": "success", "is_error": false,
        "result": "pong", "terminal_reason": "completed", "result_index": 1}),
    );
    assert_eq!(
        out,
        vec![Output::TurnDone {
            result: "pong".into(),
            is_error: false,
            reason: Some("completed".into()),
            engine_initiated: false
        }]
    );
}

#[test]
fn a_model_error_is_an_error_despite_its_success_subtype() {
    // Observed: an unknown model comes back as subtype "success".
    let out = decode(
        json!({"type": "result", "subtype": "success", "is_error": true,
        "api_error_status": 404, "terminal_reason": "api_error",
        "result": "There's an issue with the selected model (no-such-model-xyz)."}),
    );
    let Output::TurnDone {
        is_error, result, ..
    } = &out[0]
    else {
        panic!("{out:?}")
    };
    assert!(is_error);
    assert!(result.contains("no-such-model-xyz"));
}

#[test]
fn an_interrupted_turn_says_so_instead_of_claudes_diagnostic() {
    let out = decode(
        json!({"type": "result", "subtype": "error_during_execution",
        "is_error": true, "terminal_reason": "aborted_streaming",
        "errors": ["[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=null"]}),
    );
    assert_eq!(
        out,
        vec![Output::TurnDone {
            result: "Interrupted.".into(),
            is_error: true,
            reason: Some("interrupted".into()),
            engine_initiated: false
        }]
    );
}

#[test]
fn a_turn_claude_started_itself_is_marked() {
    let out = decode(
        json!({"type": "result", "subtype": "success", "is_error": false,
        "result": "The background command finished.", "origin": {"kind": "task-notification"}}),
    );
    assert!(matches!(
        out[0],
        Output::TurnDone {
            engine_initiated: true,
            ..
        }
    ));
}

#[test]
fn anything_else_passes_through() {
    for line in [
        json!({"type": "rate_limit_event"}),
        json!({"type": "system", "subtype": "hook_started"}),
        json!({"type": "control_response", "response": {"subtype": "success"}}),
        json!({"type": "user", "message": {"content": [{"type": "text", "text": "[Request interrupted by user]"}]}}),
    ] {
        assert_eq!(decode(line), vec![Output::Other]);
    }
}

// ---- what goes to Claude, in the shapes the probes confirmed ---------------

fn encoded(input: Input) -> Value {
    let lines = Claude::default().encode(&input);
    assert_eq!(lines.len(), 1);
    serde_json::from_str(&lines[0]).unwrap()
}

#[test]
fn a_user_turn_is_a_user_message() {
    assert_eq!(
        encoded(Input::User("hi".into())),
        json!({"type": "user", "message": {"role": "user", "content": "hi"}})
    );
}

#[test]
fn interrupt_is_a_control_request() {
    let v = encoded(Input::Interrupt);
    assert_eq!(v["type"], "control_request");
    assert_eq!(v["request"]["subtype"], "interrupt");
    assert!(v["request_id"].as_str().is_some_and(|s| !s.is_empty()));
}

#[test]
fn allow_echoes_the_input_and_deny_carries_the_reason() {
    let allow = encoded(Input::Approval {
        request_id: "r1".into(),
        allow: true,
        tool_input: json!({"command": "touch a"}),
        message: None,
    });
    assert_eq!(
        allow["response"],
        json!({"subtype": "success", "request_id": "r1",
               "response": {"behavior": "allow", "updatedInput": {"command": "touch a"}}})
    );
    let deny = encoded(Input::Approval {
        request_id: "r2".into(),
        allow: false,
        tool_input: json!({}),
        message: Some("no".into()),
    });
    assert_eq!(
        deny["response"]["response"],
        json!({"behavior": "deny", "message": "no"})
    );
}

#[test]
fn model_and_resume_reach_the_command_line() {
    let (program, args) = Claude::default().command(&StartOptions {
        model: Some("sonnet".into()),
        resume: Some("cc82".into()),
    });
    assert_eq!(program, "claude");
    let joined = args.join(" ");
    assert!(joined.contains("--input-format stream-json"));
    assert!(joined.contains("--permission-prompt-tool stdio"));
    assert!(joined.contains("--model sonnet"));
    assert!(joined.contains("--resume cc82"));
}
