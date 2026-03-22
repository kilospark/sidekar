use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidekarConfig {
    #[serde(default = "default_true")]
    pub telemetry: bool,
    #[serde(default = "default_true")]
    pub feedback: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
    #[serde(default = "default_true")]
    pub auto_update: bool,
    #[serde(default = "default_max_tabs")]
    pub max_tabs: usize,
}

fn default_true() -> bool {
    true
}

fn default_max_tabs() -> usize {
    20
}

impl Default for SidekarConfig {
    fn default() -> Self {
        Self {
            telemetry: true,
            feedback: true,
            browser: None,
            auto_update: true,
            max_tabs: default_max_tabs(),
        }
    }
}

pub fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".config")
        .join("sidekar")
        .join("sidekar.json")
}

pub fn load_config() -> SidekarConfig {
    let path = config_path();
    match fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
        Err(_) => SidekarConfig::default(),
    }
}

pub fn save_config(config: &SidekarConfig) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(config)?;
    fs::write(&path, json)?;
    Ok(())
}
