//! Claude Code over its stream-json protocol.
//!
//! One `claude -p` process for the whole session, JSON lines both ways.
//! Everything here was read off the real CLI, not a spec:
//!
//! - A turn is a `user` message in and a `result` out. `result.is_error` is
//!   what says it failed: a model the account cannot use comes back as
//!   `subtype: "success"`, `is_error: true`, `api_error_status: 404`.
//! - `--permission-prompt-tool stdio` turns each approval into a
//!   `control_request` (`subtype: can_use_tool`) that blocks the turn until a
//!   `control_response` arrives on stdin. Read-only tools never ask.
//! - `control_request` `subtype: interrupt` ends the running turn within about
//!   half a second, as a result with `terminal_reason: aborted_streaming`.
//! - Claude starts turns itself: when a command it moved to the background
//!   finishes, it runs another turn, and that `result` carries `origin`.
//! - `system/init` repeats at the start of every turn, with the same
//!   `session_id` throughout.

use super::engine::{Engine, Input, Output, StartOptions};
use serde_json::{Value, json};

#[derive(Default)]
pub(crate) struct Claude {
    control_seq: u64,
}

impl Engine for Claude {
    fn command(&self, opts: &StartOptions) -> (String, Vec<String>) {
        let mut args: Vec<String> = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-prompt-tool",
            "stdio",
        ]
        .map(String::from)
        .to_vec();
        if let Some(m) = &opts.model {
            args.push("--model".into());
            args.push(m.clone());
        }
        if let Some(id) = &opts.resume {
            args.push("--resume".into());
            args.push(id.clone());
        }
        ("claude".into(), args)
    }

    fn encode(&mut self, input: &Input) -> Vec<String> {
        let msg = match input {
            Input::User(text) => json!({
                "type": "user",
                "message": {"role": "user", "content": text},
            }),
            Input::Interrupt => {
                self.control_seq += 1;
                json!({
                    "type": "control_request",
                    "request_id": format!("sidekar-{}", self.control_seq),
                    "request": {"subtype": "interrupt"},
                })
            }
            Input::Approval {
                request_id,
                allow,
                tool_input,
                message,
            } => {
                let decision = if *allow {
                    json!({"behavior": "allow", "updatedInput": tool_input})
                } else {
                    json!({
                        "behavior": "deny",
                        "message": message.clone().unwrap_or_else(|| "Denied.".into()),
                    })
                };
                json!({
                    "type": "control_response",
                    "response": {
                        "subtype": "success",
                        "request_id": request_id,
                        "response": decision,
                    },
                })
            }
        };
        vec![msg.to_string()]
    }

    fn decode(&mut self, line: &Value) -> Vec<Output> {
        match line.get("type").and_then(Value::as_str) {
            Some("system") if line.get("subtype").and_then(Value::as_str) == Some("init") => {
                match line.get("session_id").and_then(Value::as_str) {
                    Some(id) => vec![Output::Ready {
                        engine_session_id: id.to_string(),
                        model: line.get("model").and_then(Value::as_str).map(String::from),
                    }],
                    None => vec![Output::Other],
                }
            }
            Some("assistant") => {
                let parts: Vec<Output> = blocks(line)
                    .iter()
                    .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                        Some("text") => b
                            .get("text")
                            .and_then(Value::as_str)
                            .map(|t| Output::Text(t.to_string())),
                        Some("tool_use") => Some(Output::ToolCall {
                            id: str_of(b, "id"),
                            name: str_of(b, "name"),
                            input: b.get("input").cloned().unwrap_or(Value::Null),
                        }),
                        _ => None,
                    })
                    .collect();
                // Thinking only: still an event, so the line reaches `raw`.
                if parts.is_empty() {
                    vec![Output::Other]
                } else {
                    parts
                }
            }
            Some("user") => {
                let results: Vec<Output> = blocks(line)
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                    .map(|b| Output::ToolResult {
                        id: str_of(b, "tool_use_id"),
                        is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                        content: content_text(b.get("content")),
                    })
                    .collect();
                if results.is_empty() {
                    vec![Output::Other]
                } else {
                    results
                }
            }
            Some("control_request") => {
                let req = line.get("request").unwrap_or(&Value::Null);
                if req.get("subtype").and_then(Value::as_str) == Some("can_use_tool") {
                    vec![Output::ApprovalRequested {
                        request_id: str_of(line, "request_id"),
                        tool: str_of(req, "tool_name"),
                        input: req.get("input").cloned().unwrap_or(Value::Null),
                    }]
                } else {
                    vec![Output::Other]
                }
            }
            Some("result") => {
                let is_error = line
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let reason = line.get("terminal_reason").and_then(Value::as_str);
                // An interrupted turn has no `result`, only an internal
                // diagnostic in `errors` that means nothing to a caller.
                let result = if reason == Some("aborted_streaming") {
                    "Interrupted.".to_string()
                } else {
                    line.get("result")
                        .and_then(Value::as_str)
                        .map(String::from)
                        .unwrap_or_else(|| content_text(line.get("errors")))
                };
                vec![Output::TurnDone {
                    result,
                    is_error,
                    reason: reason.map(|r| match r {
                        "aborted_streaming" => "interrupted".to_string(),
                        other => other.to_string(),
                    }),
                    engine_initiated: line.get("origin").is_some_and(|o| !o.is_null()),
                }]
            }
            _ => vec![Output::Other],
        }
    }
}

fn blocks(line: &Value) -> Vec<Value> {
    line.get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Text out of a content field: a string, or a list of text parts or strings.
fn content_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| match i {
                Value::String(s) => Some(s.clone()),
                other => other.get("text").and_then(Value::as_str).map(String::from),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests;
