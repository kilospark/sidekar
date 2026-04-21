//! Forward REPL `StreamEvent`s to the tunnel as structured `ch:"events"`
//! frames, so relay-side consumers (web viewer, Telegram viewer) get the
//! same event vocabulary a PTY-wrapped CLI produces via `src/events.rs`.
//!
//! Before this existed, REPL only pushed raw bytes over the tunnel; relay's
//! turn-boundary detection (which gates Telegram flushes) never fired for
//! REPL sessions. After this, REPL and PTY are symmetric from the relay's
//! point of view — both feed `ViewerMsg::Data` (bytes) and
//! `ViewerMsg::Control` (events JSON) streams.
//!
//! Symmetry note: REPL owns authoritative turn boundaries (it drives the
//! agent loop and knows when an assistant response completes), so it emits
//! `turn_start` / `assistant_complete` lifecycle frames here. The PTY
//! wrapper forwards events parsed heuristically from a third-party CLI's
//! stdout (see `src/events.rs::EventParser`) and currently does not emit
//! lifecycle markers — that's a separate concern since boundary detection
//! from raw bytes is unreliable. Content-event shape is identical across
//! both paths.

use crate::events::{AgentEvent, event_to_json, lifecycle_to_json};
use crate::providers::{ContentBlock, StreamEvent};
use crate::tunnel::tunnel_send_event;

/// State accumulated across a single assistant turn so we can emit
/// coherent content events at well-defined boundaries rather than one
/// per token delta.
#[derive(Default)]
pub(super) struct EventForwarder {
    /// True once we've emitted `turn_start` for the current turn.
    started: bool,
}

impl EventForwarder {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Forward a single `StreamEvent` to the tunnel as zero or more
    /// `ch:"events"` frames. Safe to call when no tunnel is registered
    /// (underlying helper is a no-op in that case).
    pub(super) fn forward(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::Connecting | StreamEvent::Waiting => {
                // Emit turn_start on the first "we're doing work" signal
                // of the turn. Subsequent Waiting/Connecting events are
                // the same turn (tool loop iterations).
                if !self.started {
                    self.started = true;
                    tunnel_send_event(lifecycle_to_json("turn_start"));
                }
            }
            StreamEvent::ToolExec {
                name,
                arguments_json,
            } => {
                // Emit a tool_call content event (mirrors what PTY's
                // heuristic parser produces) + a lifecycle marker.
                tunnel_send_event(event_to_json(&AgentEvent::ToolCall {
                    tool: name.clone(),
                    input: arguments_json.clone(),
                }));
                tunnel_send_event(lifecycle_to_json("tool_call_start"));
            }
            StreamEvent::Done { message } => {
                // Emit the assistant's final content as structured events
                // (text, tool calls inside the turn). Tool *results* are
                // not in this message — they come in the next user turn
                // as ToolResult blocks; we emit those elsewhere if/when
                // we wire them.
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } if !text.is_empty() => {
                            tunnel_send_event(event_to_json(&AgentEvent::Text {
                                content: text.clone(),
                            }));
                        }
                        ContentBlock::ToolCall {
                            name, arguments, ..
                        } => {
                            tunnel_send_event(event_to_json(&AgentEvent::ToolCall {
                                tool: name.clone(),
                                input: arguments.to_string(),
                            }));
                        }
                        // Thinking / EncryptedReasoning / Image / ToolResult:
                        // not surfaced as events (thinking is internal; tool
                        // results belong to the next turn; images are handled
                        // out-of-band).
                        _ => {}
                    }
                }
                // Turn-boundary marker: relay's `is_turn_boundary` uses
                // this to flush buffered output to Telegram.
                tunnel_send_event(lifecycle_to_json("assistant_complete"));
                self.started = false;
            }
            StreamEvent::Error { message } => {
                tunnel_send_event(event_to_json(&AgentEvent::Status {
                    state: format!("error: {message}"),
                }));
                // Still emit a boundary so any buffered prose flushes.
                tunnel_send_event(lifecycle_to_json("assistant_complete"));
                self.started = false;
            }
            // Deltas, thinking, ToolCallStart/Delta/End, Compacting, Idle,
            // ResolvingContext: intentionally not forwarded. They'd be
            // high-volume noise on the events channel; the Done handler
            // above captures the finalized state of the turn.
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{AssistantResponse, StopReason, Usage};
    use serde_json::Value;

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).expect("valid json")
    }

    #[test]
    fn lifecycle_emits_string_event_for_relay_boundary_detection() {
        let v = parse(&lifecycle_to_json("assistant_complete"));
        assert_eq!(v["ch"], "events");
        assert_eq!(v["v"], 1);
        assert_eq!(v["event"], "assistant_complete");
        assert!(v["event"].is_string(), "relay boundary check requires event to be a string");
    }

    #[test]
    fn content_event_has_nested_object_event() {
        let v = parse(&event_to_json(&AgentEvent::Text {
            content: "hello".into(),
        }));
        assert_eq!(v["ch"], "events");
        assert_eq!(v["event"]["kind"], "text");
        assert_eq!(v["event"]["content"], "hello");
    }

    #[test]
    fn tool_exec_emits_toolcall_then_lifecycle() {
        // We can't easily assert on tunnel_send_event's sink here (it's a
        // process-global no-op when no tunnel is registered). Instead,
        // verify the JSON shapes the forwarder would emit.
        let call = event_to_json(&AgentEvent::ToolCall {
            tool: "Bash".into(),
            input: "{\"command\":\"ls\"}".into(),
        });
        let v = parse(&call);
        assert_eq!(v["event"]["kind"], "tool_call");
        assert_eq!(v["event"]["tool"], "Bash");

        let lc = parse(&lifecycle_to_json("tool_call_start"));
        assert_eq!(lc["event"], "tool_call_start");
    }

    #[test]
    fn done_emits_text_then_assistant_complete() {
        // Also a shape test — verifies the types we'd emit from a Done.
        let response = AssistantResponse {
            content: vec![ContentBlock::Text {
                text: "result".into(),
            }],
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            model: "test".into(),
            response_id: String::new(),
        };
        let mut fwd = EventForwarder::new();
        fwd.forward(&StreamEvent::Done {
            message: response.clone(),
        });
        // Forwarder writes to the global tunnel (no-op in tests); we just
        // assert the helper functions produce the expected JSON.
        let text = parse(&event_to_json(&AgentEvent::Text {
            content: "result".into(),
        }));
        assert_eq!(text["event"]["kind"], "text");
        let boundary = parse(&lifecycle_to_json("assistant_complete"));
        assert!(boundary["event"]
            .as_str()
            .unwrap()
            .ends_with("complete"));
    }
}
