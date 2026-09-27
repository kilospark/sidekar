//! What the host needs from an engine: how to start it, how to phrase input,
//! and how to read its output. Everything else — turns, approvals policy,
//! clients — belongs to the host, so a second engine is only this.

use serde_json::Value;

/// Something for the engine.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Input {
    User(String),
    Interrupt,
    Approval {
        request_id: String,
        allow: bool,
        /// The tool input being approved; engines that echo it back need it.
        tool_input: Value,
        message: Option<String>,
    },
}

/// Something the engine said, before the host attributes it to a turn.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Output {
    Ready {
        engine_session_id: String,
        model: Option<String>,
    },
    Text(String),
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        id: String,
        is_error: bool,
        content: String,
    },
    ApprovalRequested {
        request_id: String,
        tool: String,
        input: Value,
    },
    TurnDone {
        result: String,
        is_error: bool,
        reason: Option<String>,
        /// True for a turn the engine started on its own, not one we sent.
        engine_initiated: bool,
    },
    /// Anything not modelled above.
    Other,
}

pub(crate) struct StartOptions {
    pub model: Option<String>,
    /// The engine's conversation id, to continue it rather than start fresh.
    pub resume: Option<String>,
}

pub(crate) trait Engine: Send {
    /// The program and its arguments.
    fn command(&self, opts: &StartOptions) -> (String, Vec<String>);
    /// Lines to write to the engine's stdin.
    fn encode(&mut self, input: &Input) -> Vec<String>;
    /// What one line of the engine's stdout means. The line's JSON goes into
    /// each event's `raw`; a line that is not JSON is logged and skipped.
    fn decode(&mut self, line: &Value) -> Vec<Output>;
}

pub(crate) fn for_name(name: &str) -> anyhow::Result<Box<dyn Engine>> {
    match name {
        "claude" => Ok(Box::new(super::claude::Claude::default())),
        other => anyhow::bail!("no session engine {other:?}; available: claude"),
    }
}
