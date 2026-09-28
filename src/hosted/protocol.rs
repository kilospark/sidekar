//! What goes over a session's socket: JSON lines, requests in, replies and
//! events out.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// What a session reports. Every event carries the engine's original message
/// in `raw` when there was one, so a client can reach anything the common
/// fields leave out.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub seq: u64,
    pub ts: u64,
    #[serde(flatten)]
    pub body: EventBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventBody {
    SessionStarted {
        engine: String,
    },
    /// The engine said who it is: its conversation id, for resuming.
    EngineReady {
        engine_session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// A turn began. `source` is `send` for one a client sent, `engine` for
    /// one the engine started itself — Claude runs a turn when a background
    /// task it started finishes.
    TurnStarted {
        turn_id: String,
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    Text {
        turn_id: String,
        text: String,
    },
    ToolCall {
        turn_id: String,
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        turn_id: String,
        id: String,
        is_error: bool,
        content: String,
    },
    ApprovalRequested {
        turn_id: String,
        request_id: String,
        tool: String,
        input: Value,
    },
    ApprovalResolved {
        request_id: String,
        allow: bool,
        /// `policy`, `client`, or `timeout`.
        by: String,
    },
    TurnDone {
        turn_id: String,
        result: String,
        is_error: bool,
        /// Why the turn ended, in the engine's words.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The engine was replaced by a new process continuing the same
    /// conversation, e.g. to pick up refreshed proxy credentials.
    EngineRestarted {
        reason: String,
    },
    /// A restart was needed and failed; the old engine is still in use and
    /// the restart is tried again before the next turn.
    EngineRestartFailed {
        error: String,
    },
    /// A message the adapter does not model. Nothing is dropped.
    EngineEvent {},
    SessionEnded {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
}

/// What to do with a send that arrives while a turn is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Busy {
    /// Refuse it. The default: a caller that did not think about overlap
    /// should hear about it rather than have its message land somewhere odd.
    #[default]
    Reject,
    /// Run it after the current turn.
    Queue,
    /// Stop the current turn and run this instead.
    Interrupt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Send {
        text: String,
        #[serde(default)]
        busy: Busy,
        /// Stream events from this send onward on the same connection.
        #[serde(default)]
        follow: bool,
        /// Environment for the engine from the caller, e.g. rotated proxy
        /// credentials. When the session refreshes its env, the host
        /// restarts the engine with this before the turn if it differs
        /// from what the engine was started with.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<HashMap<String, String>>,
    },
    /// Stream every event after `since`, then live ones.
    Subscribe {
        #[serde(default)]
        since: u64,
    },
    Approve {
        request_id: String,
        allow: bool,
        #[serde(default)]
        message: Option<String>,
    },
    /// Stop the running turn, if any.
    Cancel,
    Status,
    Close,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// For a send or subscribe: events after this seq follow on the stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Machine-readable reason for a refusal, e.g. `busy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Value>,
}

impl Reply {
    pub fn ok() -> Self {
        Self {
            ok: true,
            turn_id: None,
            seq: None,
            error: None,
            code: None,
            status: None,
        }
    }

    pub fn err(code: &str, error: impl Into<String>) -> Self {
        Self {
            ok: false,
            code: Some(code.into()),
            error: Some(error.into()),
            ..Self::ok()
        }
    }
}
