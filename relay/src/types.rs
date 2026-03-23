use serde::{Deserialize, Serialize};

/// Incoming registration message from a tunnel client.
#[derive(Debug, Deserialize)]
pub struct RegisterMsg {
    #[serde(rename = "type")]
    pub msg_type: String, // "register"
    pub session_name: String,
    pub agent_type: String,
    pub cwd: String,
    pub hostname: String,
}

/// Generic JSON control frame (resize, viewer events, etc.)
#[derive(Debug, Serialize, Deserialize)]
pub struct ControlMessage {
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(flatten)]
    pub data: serde_json::Value,
}

/// Session info returned in the session list API.
#[derive(Debug, Serialize, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub name: String,
    pub agent_type: String,
    pub cwd: String,
    pub hostname: String,
    pub connected_at: chrono::DateTime<chrono::Utc>,
    pub viewers: usize,
}
