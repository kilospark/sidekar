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

    /// Format the message for display in a terminal paste.
    pub fn format_for_paste(&self) -> String {
        let from = self.from.display_name();
        let reply_hint = format!("\n[reply with: sidekar bus_send {from} \"<your response>\"]");
        match self.kind {
            MessageKind::Handoff => {
                format!(
                    "[from {from}]: {} [msg_id: {}]{reply_hint}",
                    self.message,
                    self.id
                )
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
