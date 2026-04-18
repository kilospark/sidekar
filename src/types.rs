use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct SessionState {
    pub session_id: String,
    pub active_tab_id: Option<String>,
    pub tabs: Vec<String>,
    pub port: Option<u16>,
    pub host: Option<String>,
    pub browser_name: Option<String>,
    pub ref_map: Option<HashMap<String, String>>,
    pub ref_map_url: Option<String>,
    pub ref_map_timestamp: Option<i64>,
    pub prev_elements: Option<Vec<InteractiveElement>>,
    pub current_elements: Option<Vec<InteractiveElement>>,
    pub active_frame_id: Option<String>,
    pub dialog_handler: Option<DialogHandler>,
    pub block_patterns: Option<BlockPatterns>,
    pub download_dir: Option<String>,
    pub zoom_level: Option<f64>,
    pub window_id: Option<i64>,
    pub profile: Option<String>,
    pub mouse_x: Option<f64>,
    pub mouse_y: Option<f64>,
    pub mouse_buttons: Option<u8>,
    pub screencast_active: Option<bool>,
    /// If Some(true), install stealth scripts on every target of this session.
    /// `None` treated as "not set" — current default is off, opt-in via
    /// `sidekar stealth on`.
    pub stealth_enabled: Option<bool>,
    /// CDP script identifiers already installed on the current target. Used by
    /// `prepare_cdp` to skip re-registration within the same CDP session.
    pub stealth_script_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DebugTab {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub web_socket_debugger_url: Option<String>,
}

#[derive(Debug)]
pub struct BrowserCandidate {
    pub path: String,
    pub name: String,
}

#[derive(Debug)]
pub struct KeyModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

#[derive(Debug)]
pub struct KeyMapping {
    pub key: String,
    pub code: String,
    pub key_code: i64,
}

#[derive(Debug)]
pub struct LocatedElement {
    pub x: f64,
    pub y: f64,
    pub tag: String,
    pub text: String,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct DialogHandler {
    pub accept: bool,
    pub prompt_text: String,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct BlockPatterns {
    pub resource_types: Vec<String>,
    pub url_patterns: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct InteractiveElement {
    #[serde(rename = "ref")]
    pub ref_id: usize,
    pub role: String,
    pub name: String,
    pub value: String,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct ActionCacheEntry {
    pub ref_map: HashMap<String, String>,
    pub elements: Vec<InteractiveElement>,
    pub output: String,
    pub timestamp: i64,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct TabLock {
    pub session_id: String,
    pub expires: i64,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct NetworkRequestLog {
    pub id: String,
    pub method: String,
    pub url: String,
    pub req_type: String,
    pub time: i64,
    pub status: Option<i64>,
    pub status_text: Option<String>,
    pub mime_type: Option<String>,
    pub post_data: Option<String>,
    pub request_headers: Option<Vec<(String, String)>>,
    pub response_headers: Option<Vec<(String, String)>>,
    pub response_size: Option<i64>,
    pub started_date_time: Option<String>,
    pub time_ms: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CommandFileEntry {
    pub command: String,
    #[serde(default)]
    pub args: Vec<Value>,
}
