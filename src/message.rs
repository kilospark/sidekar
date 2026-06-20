//! Typed message model for sidekar agent communication.
//!
//! All message types are transport-independent. The [`Envelope`] is the core
//! unit of communication; [`AgentId`] identifies any agent regardless of
//! how it is reached.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Agent identity
// ---------------------------------------------------------------------------

/// Transport-independent agent identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentId {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nick: Option<String>,
    /// Logical channel (directory path or user-set name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Transport-specific locator (pane ID, agent name, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
    /// Agent system type: "sidekar", "agentbus", etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
}

impl AgentId {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            nick: None,
            session: None,
            pane: None,
            agent_type: None,
        }
    }

    /// Human-readable label: `nick(name)` when nick is set, otherwise just `name`.
    pub fn display_name(&self) -> String {
        match &self.nick {
            Some(n) => format!("{n}({})", self.name),
            None => self.name.clone(),
        }
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

// ---------------------------------------------------------------------------
// Message kinds
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    Request,
    Response,
    Fyi,
    Handoff,
}

impl MessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
            Self::Fyi => "fyi",
            Self::Handoff => "handoff",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "request" => Self::Request,
            "response" => Self::Response,
            "handoff" => Self::Handoff,
            _ => Self::Fyi,
        }
    }
}

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// Transport-independent message envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub id: String,
    pub from: AgentId,
    pub to: String,
    pub kind: MessageKind,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    pub created_at: u64,
}

impl Envelope {
    pub fn new(
        from: AgentId,
        to: impl Into<String>,
        kind: MessageKind,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id: gen_msg_id(),
            from,
            to: to.into(),
            kind,
            message: message.into(),
            summary: None,
            request: None,
            reply_to: None,
            created_at: epoch_secs(),
        }
    }

    pub fn new_request(from: AgentId, to: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(from, to, MessageKind::Request, message)
    }

    pub fn new_response(
        from: AgentId,
        to: impl Into<String>,
        message: impl Into<String>,
        reply_to: String,
    ) -> Self {
        let mut env = Self::new(from, to, MessageKind::Response, message);
        env.reply_to = Some(reply_to);
        env
    }

    pub fn new_fyi(from: AgentId, to: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(from, to, MessageKind::Fyi, message)
    }

    pub fn new_handoff(
        from: AgentId,
        to: impl Into<String>,
        summary: impl Into<String>,
        request: impl Into<String>,
    ) -> Self {
        let summary = summary.into();
        let request = request.into();
        let message = format!("{summary} Request: {request}");
        let mut env = Self::new(from, to, MessageKind::Handoff, message);
        env.summary = Some(summary);
        env.request = Some(request);
        env
    }

    /// True when this envelope expects a reciprocal bus reply (pending + reply hint).
    pub fn requires_reply(&self) -> bool {
        match self.kind {
            MessageKind::Handoff => true,
            MessageKind::Request => !is_terminal_ack(&self.message),
            _ => false,
        }
    }

    /// Format the message for display in a terminal paste.
    pub fn format_for_paste(&self) -> String {
        let from = self.from.display_name();
        let reply_hint = format!(
            "\n[reply with: sidekar bus send {from} \"<your response>\" --reply-to={}]",
            self.id
        );
        match self.kind {
            MessageKind::Handoff => {
                format!(
                    "[from {from}]: {} [msg_id: {}]{reply_hint}",
                    self.message, self.id
                )
            }
            MessageKind::Request if !self.requires_reply() => {
                format!("[fyi from {from}]: {}", self.message)
            }
            MessageKind::Request => {
                format!("[request from {from}]: {}{reply_hint}", self.message)
            }
            MessageKind::Fyi => {
                format!("[fyi from {from}]: {}", self.message)
            }
            MessageKind::Response => {
                format!("[response from {from}]: {}", self.message)
            }
        }
    }

    /// Short preview of the message content (max 100 chars).
    pub fn preview(&self) -> &str {
        let msg = if !self.message.is_empty() {
            &self.message
        } else {
            self.request.as_deref().unwrap_or("")
        };
        if msg.len() <= 100 {
            return msg;
        }
        let mut end = 100;
        while end > 0 && !msg.is_char_boundary(end) {
            end -= 1;
        }
        &msg[..end]
    }
}

// ---------------------------------------------------------------------------
// Delivery result
// ---------------------------------------------------------------------------

/// Outcome of a transport delivery attempt.
#[derive(Debug)]
pub enum DeliveryResult {
    /// Message was delivered and confirmed.
    Delivered,
    /// Message was accepted but delivery is unconfirmed.
    Queued,
    /// Delivery failed.
    Failed(String),
}

impl DeliveryResult {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Delivered | Self::Queued)
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum MessageError {
    AgentNotFound(String),
    NotRegistered,
    TransportFailed(String),
    TooLarge { size: usize, max: usize },
}

impl fmt::Display for MessageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AgentNotFound(name) => write!(f, "agent \"{name}\" not found"),
            Self::NotRegistered => write!(f, "not registered on the bus"),
            Self::TransportFailed(reason) => write!(f, "transport failed: {reason}"),
            Self::TooLarge { size, max } => {
                write!(f, "message too large ({size} bytes, max {max})")
            }
        }
    }
}

impl std::error::Error for MessageError {}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn gen_msg_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let r: u16 = rand::random();
    format!("{:x}-{:04x}", ts & 0xFFFF_FFFF, r)
}

/// Short closing/ack text mis-sent as `request` should not open a reply loop.
pub fn is_terminal_ack(message: &str) -> bool {
    let msg = message.trim();
    if msg.is_empty() || msg.len() > 60 {
        return false;
    }
    if msg == "👍" || msg == "—" || msg == "-" || msg == "." {
        return true;
    }
    const TOKENS: &[&str] = &[
        "ok", "okay", "closed", "close", "done", "ack", "thanks", "thank", "you", "thx", "roger",
        "yes", "yep", "no", "nope",
    ];
    let lower = msg.to_ascii_lowercase();
    if TOKENS.contains(&lower.as_str()) {
        return true;
    }
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .all(|word| TOKENS.contains(&word))
}

/// If `body` is a pasted terminal-ack request, return its msg_id for dismissal.
pub fn terminal_ack_msg_id_from_paste(body: &str) -> Option<String> {
    let message = body
        .strip_prefix("[request from ")
        .and_then(|rest| rest.split("]: ").nth(1))
        .and_then(|rest| rest.split("\n[reply with:").next())?;
    if !is_terminal_ack(message) {
        return None;
    }
    body.split("--reply-to=")
        .nth(1)
        .map(|tail| tail.split(['\n', ' ', ']']).next().unwrap_or(tail).to_string())
        .filter(|id| !id.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_ack_detects_closing_phrases() {
        for msg in [
            "closed",
            "ok",
            "ok. closed.",
            "ack. closed.",
            "👍",
            "—",
            ".",
            "thanks",
        ] {
            assert!(is_terminal_ack(msg), "expected terminal ack: {msg}");
        }
        assert!(!is_terminal_ack(
            "Please review the PR and reply with findings."
        ));
    }

    #[test]
    fn terminal_ack_request_formats_as_fyi_without_reply_hint() {
        let env = Envelope::new_request(AgentId::new("quokka"), "toucan", "closed.");
        let paste = env.format_for_paste();
        assert!(paste.starts_with("[fyi from quokka]: closed."));
        assert!(!paste.contains("[reply with:"));
        assert!(!env.requires_reply());
    }

    #[test]
    fn terminal_ack_paste_extracts_msg_id() {
        let paste = "[request from quokka]: closed.\n[reply with: sidekar bus send quokka \"x\" --reply-to=e1e3521c-3cdd]";
        assert_eq!(
            terminal_ack_msg_id_from_paste(paste).as_deref(),
            Some("e1e3521c-3cdd")
        );
    }
}
