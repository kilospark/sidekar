use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopAppInfo {
    pub pid: i32,
    pub bundle_id: Option<String>,
    pub name: String,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopWindowInfo {
    pub pid: i32,
    pub window_id: Option<u32>,
    pub title: Option<String>,
    pub frame: DesktopRect,
    pub is_main: bool,
    pub is_focused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopElementStep {
    pub role: String,
    pub title: Option<String>,
    pub identifier: Option<String>,
    pub index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopElementPath {
    pub pid: i32,
    pub chain: Vec<DesktopElementStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopElementMatch {
    pub path: DesktopElementPath,
    pub role: String,
    pub title: Option<String>,
    pub value: Option<String>,
    pub frame: Option<DesktopRect>,
    pub actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopClickResult {
    pub kind: String,
    pub role: Option<String>,
    pub title: Option<String>,
    pub x: Option<f64>,
    pub y: Option<f64>,
}
