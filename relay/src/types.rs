use serde::{Deserialize, Serialize};

/// Incoming registration message from a tunnel client.
#[derive(Debug, Deserialize)]
pub struct RegisterMsg {
    #[serde(rename = "type")]
    pub _msg_type: String, // "register"
    pub session_name: String,
    pub agent_type: String,
    pub cwd: String,
    pub hostname: String,
    #[serde(default)]
    pub nickname: Option<String>,
    /// Protocol version. Current tunnel clients use 2 (multiplex bus JSON on text frames).
    pub proto: u8,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
}

/// Session info returned in the session list API.
#[derive(Debug, Serialize, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub name: String,
    pub agent_type: String,
    pub cwd: String,
    pub hostname: String,
    pub nickname: Option<String>,
    pub owner_origin: Option<String>,
    pub connected_at: chrono::DateTime<chrono::Utc>,
    pub viewers: usize,
}
