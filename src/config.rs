use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Once;

/// All known config keys with their types and defaults.
/// This is the single source of truth for config schema.
pub struct ConfigKey {
    pub key: &'static str,
    pub kind: ConfigKind,
    pub default: &'static str,
    pub description: &'static str,
}

pub enum ConfigKind {
    Bool,
    Int,
    String,
}

pub static CONFIG_KEYS: &[ConfigKey] = &[
    ConfigKey { key: "telemetry", kind: ConfigKind::Bool, default: "true", description: "Send anonymous usage counts" },
    ConfigKey { key: "feedback", kind: ConfigKind::Bool, default: "true", description: "Allow sending feedback" },
    ConfigKey { key: "browser", kind: ConfigKind::String, default: "", description: "Preferred browser (chrome, edge, brave, arc, vivaldi, chromium, canary)" },
    ConfigKey { key: "auto_update", kind: ConfigKind::Bool, default: "true", description: "Auto-update on PTY launch" },
    ConfigKey { key: "max_tabs", kind: ConfigKind::Int, default: "20", description: "Maximum open tabs per session" },
    ConfigKey { key: "cdp_timeout_secs", kind: ConfigKind::Int, default: "60", description: "CDP command timeout in seconds" },
    ConfigKey { key: "max_cron_jobs", kind: ConfigKind::Int, default: "10", description: "Maximum cron jobs" },
];

pub fn find_key(key: &str) -> Option<&'static ConfigKey> {
    CONFIG_KEYS.iter().find(|k| k.key == key)
}

/// The struct is kept for in-memory convenience. It's populated from SQLite.
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
    #[serde(default = "default_cdp_timeout")]
    pub cdp_timeout_secs: u64,
    #[serde(default = "default_max_cron_jobs")]
    pub max_cron_jobs: usize,
}

fn default_true() -> bool { true }
fn default_max_tabs() -> usize { 20 }
fn default_cdp_timeout() -> u64 { 60 }
fn default_max_cron_jobs() -> usize { 10 }

impl Default for SidekarConfig {
    fn default() -> Self {
        Self {
            telemetry: true,
            feedback: true,
            browser: None,
            auto_update: true,
            max_tabs: default_max_tabs(),
            cdp_timeout_secs: default_cdp_timeout(),
            max_cron_jobs: default_max_cron_jobs(),
        }
    }
}

// ---------------------------------------------------------------------------
// Legacy JSON path (for migration + first-run detection)
// ---------------------------------------------------------------------------

fn legacy_json_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".config")
        .join("sidekar")
        .join("sidekar.json")
}

/// Still used by first-run detection in main.rs
pub fn config_path() -> PathBuf {
    crate::broker::db_path()
}

pub fn is_first_run() -> bool {
    // First run if neither the SQLite DB nor the legacy JSON exist
    !crate::broker::db_path().exists() && !legacy_json_path().exists()
}

// ---------------------------------------------------------------------------
// One-time migration from JSON → SQLite
// ---------------------------------------------------------------------------

static MIGRATION: Once = Once::new();

fn migrate_json_if_needed() {
    MIGRATION.call_once(|| {
        let path = legacy_json_path();
        if !path.exists() {
            return;
        }
        // Check if we already migrated (any config row exists)
        if let Ok(conn) = crate::broker::open_db() {
            let has_rows: bool = conn
                .query_row("SELECT COUNT(*) > 0 FROM config", [], |r| r.get(0))
                .unwrap_or(false);
            if has_rows {
                return; // already migrated
            }
        }
        // Read legacy JSON and import
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(old) = serde_json::from_str::<SidekarConfig>(&contents) {
                let _ = config_set("telemetry", &old.telemetry.to_string());
                let _ = config_set("feedback", &old.feedback.to_string());
                if let Some(ref b) = old.browser {
                    let _ = config_set("browser", b);
                }
                let _ = config_set("auto_update", &old.auto_update.to_string());
                let _ = config_set("max_tabs", &old.max_tabs.to_string());
                let _ = config_set("cdp_timeout_secs", &old.cdp_timeout_secs.to_string());
                let _ = config_set("max_cron_jobs", &old.max_cron_jobs.to_string());
                eprintln!("sidekar: migrated config from {} to SQLite", path.display());
            }
        }
    });
}

// ---------------------------------------------------------------------------
// SQLite-backed config get/set
// ---------------------------------------------------------------------------

/// Get a single config value, returning the default if not set.
pub fn config_get(key: &str) -> String {
    migrate_json_if_needed();
    if let Ok(conn) = crate::broker::open_db() {
        if let Ok(val) = conn.query_row(
            "SELECT value FROM config WHERE key = ?1",
            [key],
            |r| r.get::<_, String>(0),
        ) {
            return val;
        }
    }
    // Return default
    find_key(key).map(|k| k.default.to_string()).unwrap_or_default()
}

/// Set a config value. Returns error if key is unknown.
pub fn config_set(key: &str, value: &str) -> Result<()> {
    migrate_json_if_needed();
    let conn = crate::broker::open_db()?;
    conn.execute(
        "INSERT INTO config (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

/// Delete a config key (revert to default).
pub fn config_delete(key: &str) -> Result<()> {
    let conn = crate::broker::open_db()?;
    conn.execute("DELETE FROM config WHERE key = ?1", [key])?;
    Ok(())
}

/// Get all config values (including defaults for unset keys).
pub fn config_list() -> Vec<(String, String, bool)> {
    migrate_json_if_needed();
    let mut set_values = std::collections::HashMap::new();
    if let Ok(conn) = crate::broker::open_db() {
        if let Ok(mut stmt) = conn.prepare("SELECT key, value FROM config") {
            if let Ok(rows) = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            }) {
                for row in rows.flatten() {
                    set_values.insert(row.0, row.1);
                }
            }
        }
    }
    CONFIG_KEYS
        .iter()
        .map(|k| {
            let (val, is_default) = match set_values.get(k.key) {
                Some(v) => (v.clone(), false),
                None => (k.default.to_string(), true),
            };
            (k.key.to_string(), val, is_default)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Build SidekarConfig struct from SQLite (for existing call sites)
// ---------------------------------------------------------------------------

fn get_bool(key: &str) -> bool {
    let v = config_get(key);
    v == "true" || v == "1"
}

fn get_usize(key: &str) -> usize {
    config_get(key).parse().unwrap_or_else(|_| {
        find_key(key)
            .map(|k| k.default.parse().unwrap_or(0))
            .unwrap_or(0)
    })
}

fn get_u64(key: &str) -> u64 {
    config_get(key).parse().unwrap_or_else(|_| {
        find_key(key)
            .map(|k| k.default.parse().unwrap_or(0))
            .unwrap_or(0)
    })
}

pub fn load_config() -> SidekarConfig {
    migrate_json_if_needed();
    let browser_val = config_get("browser");
    SidekarConfig {
        telemetry: get_bool("telemetry"),
        feedback: get_bool("feedback"),
        browser: if browser_val.is_empty() { None } else { Some(browser_val) },
        auto_update: get_bool("auto_update"),
        max_tabs: get_usize("max_tabs"),
        cdp_timeout_secs: get_u64("cdp_timeout_secs"),
        max_cron_jobs: get_usize("max_cron_jobs"),
    }
}

/// Save all fields from a SidekarConfig struct (backwards compat).
pub fn save_config(config: &SidekarConfig) -> Result<()> {
    config_set("telemetry", &config.telemetry.to_string())?;
    config_set("feedback", &config.feedback.to_string())?;
    config_set("browser", config.browser.as_deref().unwrap_or(""))?;
    config_set("auto_update", &config.auto_update.to_string())?;
    config_set("max_tabs", &config.max_tabs.to_string())?;
    config_set("cdp_timeout_secs", &config.cdp_timeout_secs.to_string())?;
    config_set("max_cron_jobs", &config.max_cron_jobs.to_string())?;
    Ok(())
}
