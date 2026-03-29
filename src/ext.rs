//! Extension bridge for Chrome extension communication.
//!
//! The Chrome extension connects through native messaging, and the daemon
//! routes `sidekar ext <command>` requests to the connected extension bridge.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::auth;

const DEFAULT_API_URL: &str = "https://sidekar.dev";
const OFFICIAL_EXTENSION_ID: &str = "ieggclnoffcnljcjeadgogpfbnhogncc";

const TIMEOUT_SECS: u64 = 30;

fn data_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".sidekar")
}

fn sidekar_profile_roots() -> Vec<PathBuf> {
    let profiles_root = data_dir().join("profiles");
    let entries = match std::fs::read_dir(&profiles_root) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_dir())
        .collect()
}

// ---------------------------------------------------------------------------
// Shared state between server and extension connection
// ---------------------------------------------------------------------------

pub struct ExtState {
    pub bridge_tx: Option<mpsc::UnboundedSender<String>>,
    pub pending: HashMap<String, oneshot::Sender<Value>>,
    pub connected: bool,
    pub authenticated: bool,
    pub verified_user_id: Option<String>,
    pub connection_id: u64,
}

impl Default for ExtState {
    fn default() -> Self {
        Self {
            bridge_tx: None,
            pending: HashMap::new(),
            connected: false,
            authenticated: false,
            verified_user_id: None,
            connection_id: 0,
        }
    }
}

pub type SharedExtState = Arc<Mutex<ExtState>>;

fn is_sidekar_extension_entry(ext_id: &str, meta: &Value) -> bool {
    if ext_id == OFFICIAL_EXTENSION_ID {
        return true;
    }

    let manifest = meta.get("manifest").and_then(Value::as_object);
    let name = manifest
        .and_then(|m| m.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let description = manifest
        .and_then(|m| m.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let path = meta
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default();

    name == "Sidekar"
        || description == "Bridge between AI agents and your browser"
        || path.ends_with("/extension")
        || path.ends_with("\\extension")
}

#[cfg(target_os = "macos")]
fn chrome_profile_root() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow!("Cannot find home directory"))?
        .join("Library/Application Support/Google/Chrome"))
}

#[cfg(target_os = "linux")]
fn chrome_profile_root() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow!("Cannot find home directory"))?
        .join(".config/google-chrome"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn chrome_profile_root() -> Result<PathBuf> {
    bail!("Native messaging host installation not supported on this OS")
}

fn discover_sidekar_extension_ids() -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    let root = chrome_profile_root()?;
    if !root.is_dir() {
        return Ok(ids);
    }

    for entry in std::fs::read_dir(&root)? {
        let entry = match entry {
            Ok(v) => v,
            Err(_) => continue,
        };
        let profile_dir = entry.path();
        if !profile_dir.is_dir() {
            continue;
        }

        for prefs_name in ["Preferences", "Secure Preferences"] {
            let prefs_path = profile_dir.join(prefs_name);
            if !prefs_path.is_file() {
                continue;
            }
            let raw = match std::fs::read_to_string(&prefs_path) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let data: Value = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let settings = match data
                .get("extensions")
                .and_then(|v| v.get("settings"))
                .and_then(Value::as_object)
            {
                Some(v) => v,
                None => continue,
            };
            for (ext_id, meta) in settings {
                if is_sidekar_extension_entry(ext_id, meta) {
                    ids.insert(ext_id.clone());
                }
            }
        }
    }

    Ok(ids)
}

fn read_existing_allowed_origins(manifest_path: &std::path::Path) -> BTreeSet<String> {
    let raw = match std::fs::read_to_string(manifest_path) {
        Ok(v) => v,
        Err(_) => return BTreeSet::new(),
    };
    let value: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return BTreeSet::new(),
    };
    value
        .get("allowed_origins")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn native_host_manifest_dirs() -> Result<Vec<PathBuf>> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Cannot find home directory"))?;
    let mut dirs = vec![
        home.join("Library/Application Support/Google/Chrome/NativeMessagingHosts"),
        home.join("Library/Application Support/Google/ChromeForTesting/NativeMessagingHosts"),
        home.join("Library/Application Support/Google/Chrome Canary/NativeMessagingHosts"),
        home.join("Library/Application Support/Chromium/NativeMessagingHosts"),
        home.join("Library/Application Support/BraveSoftware/Brave-Browser/NativeMessagingHosts"),
        home.join("Library/Application Support/Microsoft Edge/NativeMessagingHosts"),
    ];
    dirs.extend(
        sidekar_profile_roots()
            .into_iter()
            .map(|root| root.join("NativeMessagingHosts")),
    );
    Ok(dirs)
}

#[cfg(target_os = "linux")]
fn native_host_manifest_dirs() -> Result<Vec<PathBuf>> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Cannot find home directory"))?;
    let mut dirs = vec![
        home.join(".config/google-chrome/NativeMessagingHosts"),
        home.join(".config/google-chrome-beta/NativeMessagingHosts"),
        home.join(".config/google-chrome-for-testing/NativeMessagingHosts"),
        home.join(".config/chromium/NativeMessagingHosts"),
        home.join(".config/BraveSoftware/Brave-Browser/NativeMessagingHosts"),
        home.join(".config/microsoft-edge/NativeMessagingHosts"),
    ];
    dirs.extend(
        sidekar_profile_roots()
            .into_iter()
            .map(|root| root.join("NativeMessagingHosts")),
    );
    Ok(dirs)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn native_host_manifest_dirs() -> Result<Vec<PathBuf>> {
    bail!("Native messaging host installation not supported on this OS")
}

fn native_host_allowed_origins(manifest_paths: &[PathBuf], extension_id: Option<&str>) -> Result<Vec<String>> {
    let mut ids = BTreeSet::new();
    for manifest_path in manifest_paths {
        for origin in read_existing_allowed_origins(manifest_path) {
            if let Some(id) = origin
                .strip_prefix("chrome-extension://")
                .and_then(|s| s.strip_suffix('/'))
            {
                if !id.trim().is_empty() {
                    ids.insert(id.to_string());
                }
            }
        }
    }

    ids.insert(OFFICIAL_EXTENSION_ID.to_string());
    if let Some(id) = extension_id.map(str::trim).filter(|s| !s.is_empty()) {
        ids.insert(id.to_string());
    }

    ids.extend(discover_sidekar_extension_ids()?);

    Ok(ids
        .into_iter()
        .map(|id| format!("chrome-extension://{id}/"))
        .collect())
}

fn ext_api_base() -> String {
    std::env::var("SIDEKAR_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string())
}

fn verify_ext_token_sync(ext_token: &str) -> Result<String> {
    let device_token = auth::auth_token().ok_or_else(|| anyhow!("Run `sidekar login`"))?;

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let url = format!("{}/api/auth/ext-token?verify=1", ext_api_base());
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", device_token))
        .json(&json!({ "ext_token": ext_token }))
        .send()
        .context("Failed to contact sidekar.dev for token verification")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        bail!("Token verification failed: HTTP {status} — {body}");
    }

    let data: Value = resp.json().context("Invalid response from verify-ext")?;

    let matched = data.get("match").and_then(|v| v.as_bool()).unwrap_or(false);
    if !matched {
        bail!("Extension token and CLI token belong to different users");
    }

    data.get("user_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("No user_id in verification response"))
}

/// Verify that the extension token and CLI device token belong to the same user.
/// Calls the sidekar.dev API and returns the user_id on success.
async fn verify_ext_token(ext_token: &str) -> Result<String> {
    let device_token = auth::auth_token().ok_or_else(|| anyhow!("Run `sidekar login`"))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let url = format!("{}/api/auth/ext-token?verify=1", ext_api_base());
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", device_token))
        .json(&json!({ "ext_token": ext_token }))
        .send()
        .await
        .context("Failed to contact sidekar.dev for token verification")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("Token verification failed: HTTP {status} — {body}");
    }

    let data: Value = resp
        .json()
        .await
        .context("Invalid response from verify-ext")?;

    let matched = data.get("match").and_then(|v| v.as_bool()).unwrap_or(false);
    if !matched {
        bail!("Extension token and CLI token belong to different users");
    }

    data.get("user_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("No user_id in verification response"))
}

type SharedState = Arc<Mutex<ExtState>>;

// ---------------------------------------------------------------------------
// Ext bridge for daemon
// ---------------------------------------------------------------------------

async fn disconnect_bridge(state: &SharedState, connection_id: u64) {
    let pending = {
        let mut s = state.lock().await;
        if s.connection_id != connection_id {
            return;
        }
        let pending = std::mem::take(&mut s.pending);
        s.bridge_tx = None;
        s.connected = false;
        s.authenticated = false;
        s.verified_user_id = None;
        pending
    };
    for (_id, tx) in pending {
        let _ = tx.send(json!({"error": "Extension disconnected"}));
    }
}

pub async fn register_bridge_connection(
    state: SharedState,
    mut reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    mut writer: tokio::net::unix::OwnedWriteHalf,
    user_id: String,
) -> Result<()> {
    let (bridge_tx, mut bridge_rx) = mpsc::unbounded_channel::<String>();
    let (connection_id, replaced_pending) = {
        let mut s = state.lock().await;
        let replaced_pending = std::mem::take(&mut s.pending);
        s.connection_id = s.connection_id.wrapping_add(1);
        s.bridge_tx = Some(bridge_tx);
        s.connected = true;
        s.authenticated = true;
        s.verified_user_id = Some(user_id);
        (s.connection_id, replaced_pending)
    };
    for (_id, tx) in replaced_pending {
        let _ = tx.send(json!({"error": "Extension bridge replaced by a new connection"}));
    }

    writer.write_all(b"{\"ok\":true}\n").await?;
    writer.flush().await?;

    let write_state = state.clone();
    tokio::spawn(async move {
        while let Some(msg) = bridge_rx.recv().await {
            if writer.write_all(msg.as_bytes()).await.is_err() {
                break;
            }
            if writer.write_all(b"\n").await.is_err() {
                break;
            }
            if writer.flush().await.is_err() {
                break;
            }
        }
        disconnect_bridge(&write_state, connection_id).await;
    });

    let mut line = String::new();
    while let Ok(n) = reader.read_line(&mut line).await {
        if n == 0 {
            break;
        }
        if let Ok(val) = serde_json::from_str::<Value>(line.trim()) {
            if let Some(id) = val.get("id").and_then(|v| v.as_str()) {
                let mut s = state.lock().await;
                if let Some(tx) = s.pending.remove(id) {
                    let _ = tx.send(val);
                }
            }
        }
        line.clear();
    }

    disconnect_bridge(&state, connection_id).await;
    Ok(())
}

/// Send a command to the extension via the shared state.
/// Used by daemon to forward ext commands from unix socket.
pub async fn forward_command(state: &SharedExtState, command: Value) -> Value {
    match send_command(state, command).await {
        Ok(v) => v,
        Err(e) => json!({"error": e.to_string()}),
    }
}

/// Get extension connection status.
pub async fn get_status(state: &SharedExtState) -> Value {
    let s = state.lock().await;
    json!({
        "connected": s.connected,
        "authenticated": s.authenticated,
        "user_id": s.verified_user_id,
    })
}

async fn send_command(state: &SharedState, command: Value) -> Result<Value> {
    let id = format!("{:08x}", rand::random::<u32>());
    let mut msg = command;
    msg.as_object_mut().unwrap().insert("id".into(), json!(id));

    let (tx, rx) = oneshot::channel();

    {
        let mut s = state.lock().await;
        if !s.connected || !s.authenticated || s.bridge_tx.is_none() {
            bail!("Extension not connected. Is Chrome running with the Sidekar extension?");
        }
        s.pending.insert(id.clone(), tx);
        let text = serde_json::to_string(&msg)?;
        if let Some(ref bridge_tx) = s.bridge_tx {
            if bridge_tx.send(text).is_err() {
                s.pending.remove(&id);
                bail!("Failed to send to extension bridge");
            }
        }
    }

    match tokio::time::timeout(std::time::Duration::from_secs(TIMEOUT_SECS), rx).await {
        Ok(Ok(val)) => Ok(val),
        Ok(Err(_)) => bail!("Extension response channel closed"),
        Err(_) => {
            state.lock().await.pending.remove(&id);
            bail!("Extension command timed out ({TIMEOUT_SECS}s)")
        }
    }
}

/// Check if the extension is connected and authenticated (blocking, 500ms max).
///
/// Used by the auto-routing logic in main.rs to decide whether browser commands
/// should be routed through the Chrome extension instead of CDP.
pub fn is_ext_available() -> bool {
    if !crate::daemon::is_running() {
        return false;
    }
    crate::daemon::send_command(&json!({"type": "ext_status"}))
        .ok()
        .map(|val| {
            val.get("connected")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                && val
                    .get("authenticated")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// CLI client
// ---------------------------------------------------------------------------

pub async fn send_cli_command(
    command: &str,
    args: &[String],
    default_tab: Option<u64>,
) -> Result<()> {
    // Handle meta commands
    if command == "stop" {
        return crate::daemon::stop();
    }
    if command == "status" {
        return show_status();
    }

    let msg = build_command(command, args, default_tab)?;
    crate::daemon::ensure_running()?;
    let result = crate::daemon::send_command(&json!({
        "type": "ext",
        "command": msg,
    }))?;

    if let Some(err) = result.get("error").and_then(|v| v.as_str()) {
        bail!("{err}");
    }

    print_result(command, &result);
    Ok(())
}

fn show_status() -> Result<()> {
    if !crate::daemon::is_running() {
        println!("Extension bridge not running");
        return Ok(());
    }

    let status = crate::daemon::send_command(&json!({"type": "ext_status"}))?;
    let connected = status
        .get("connected")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let authenticated = status
        .get("authenticated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    println!(
        "Extension bridge: {}",
        if connected { "connected" } else { "not connected" }
    );
    println!(
        "Authenticated: {}",
        if authenticated { "yes" } else { "no" }
    );
    Ok(())
}

fn build_command(command: &str, args: &[String], default_tab: Option<u64>) -> Result<Value> {
    // Explicit tab id in subcommand args wins over global `--tab`.
    fn tab_from_arg_or_default(explicit: Option<u64>, default_tab: Option<u64>) -> Option<u64> {
        explicit.or(default_tab)
    }

    match command {
        "tabs" => Ok(json!({"command": "tabs"})),
        "read" => {
            let tab_id = tab_from_arg_or_default(
                args.first().and_then(|s| s.parse::<u64>().ok()),
                default_tab,
            );
            let mut cmd = json!({"command": "read"});
            if let Some(id) = tab_id {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "screenshot" => {
            let tab_id = tab_from_arg_or_default(
                args.first().and_then(|s| s.parse::<u64>().ok()),
                default_tab,
            );
            let mut cmd = json!({"command": "screenshot"});
            if let Some(id) = tab_id {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "click" => {
            let target = args
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("Usage: sidekar ext click <selector|text:...>"))?;
            let mut cmd = json!({"command": "click", "target": target});
            if let Some(id) = default_tab {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "type" => {
            if args.len() < 2 {
                bail!("Usage: sidekar ext type <selector> <text>");
            }
            let mut cmd =
                json!({"command": "type", "selector": args[0], "text": args[1..].join(" ")});
            if let Some(id) = default_tab {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "paste" => {
            let mut html: Option<String> = None;
            let mut text: Option<String> = None;
            let mut selector: Option<String> = None;
            let mut plain_parts = Vec::new();
            let mut i = 0;
            while i < args.len() {
                match args[i].as_str() {
                    "--html" => {
                        i += 1;
                        let value = args.get(i).cloned().context(
                            "Usage: sidekar ext paste [--html <html>] [--text <text>] [--selector <selector>]",
                        )?;
                        html = Some(value);
                    }
                    "--text" => {
                        i += 1;
                        let value = args.get(i).cloned().context(
                            "Usage: sidekar ext paste [--html <html>] [--text <text>] [--selector <selector>]",
                        )?;
                        text = Some(value);
                    }
                    "--selector" => {
                        i += 1;
                        let value = args.get(i).cloned().context(
                            "Usage: sidekar ext paste [--html <html>] [--text <text>] [--selector <selector>]",
                        )?;
                        selector = Some(value);
                    }
                    other => plain_parts.push(other.to_string()),
                }
                i += 1;
            }
            if text.is_none() && !plain_parts.is_empty() {
                text = Some(plain_parts.join(" "));
            }
            if text.as_deref().unwrap_or("").is_empty() && html.as_deref().unwrap_or("").is_empty() {
                bail!(
                    "Usage: sidekar ext paste [--html <html>] [--text <text>] [--selector <selector>]"
                );
            }
            let mut cmd = json!({"command": "paste", "text": text.unwrap_or_default()});
            if let Some(html) = html {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("html".into(), json!(html));
            }
            if let Some(selector) = selector {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("selector".into(), json!(selector));
            }
            if let Some(id) = default_tab {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "setvalue" => {
            if args.len() < 2 {
                bail!("Usage: sidekar ext setvalue <selector> <text>");
            }
            let mut cmd =
                json!({"command": "setvalue", "selector": args[0], "text": args[1..].join(" ")});
            if let Some(id) = default_tab {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "axtree" => {
            let tab_id = tab_from_arg_or_default(
                args.first().and_then(|s| s.parse::<u64>().ok()),
                default_tab,
            );
            let mut cmd = json!({"command": "axtree"});
            if let Some(id) = tab_id {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "eval" => {
            let code = args.join(" ");
            if code.is_empty() {
                bail!("Usage: sidekar ext eval <javascript>");
            }
            let mut cmd = json!({"command": "eval", "code": code});
            if let Some(id) = default_tab {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "evalpage" => {
            let code = args.join(" ");
            if code.is_empty() {
                bail!("Usage: sidekar ext evalpage <javascript>");
            }
            let mut cmd = json!({"command": "evalpage", "code": code});
            if let Some(id) = default_tab {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "navigate" => {
            if args.is_empty() {
                bail!("Usage: sidekar ext navigate <url> [tab_id]");
            }
            let url = &args[0];
            let tab_id = tab_from_arg_or_default(
                args.get(1).and_then(|s| s.parse::<u64>().ok()),
                default_tab,
            );
            let mut cmd = json!({"command": "navigate", "url": url});
            if let Some(id) = tab_id {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "newtab" => {
            let url = args
                .first()
                .cloned()
                .unwrap_or_else(|| "about:blank".to_string());
            Ok(json!({"command": "newtab", "url": url}))
        }
        "close" => {
            let tab_id = tab_from_arg_or_default(
                args.first().and_then(|s| s.parse::<u64>().ok()),
                default_tab,
            );
            let mut cmd = json!({"command": "close"});
            if let Some(id) = tab_id {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "scroll" => {
            let direction = args.first().map(|s| s.as_str()).unwrap_or("down");
            let mut cmd = json!({"command": "scroll", "direction": direction});
            if let Some(id) = default_tab {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        _ => bail!(
            "Unknown ext command: {command}\nAvailable: tabs, read, screenshot, click, type, paste, setvalue, axtree, eval, evalpage, navigate, newtab, close, scroll, status, stop"
        ),
    }
}

fn print_result(command: &str, result: &Value) {
    match command {
        "tabs" => {
            if let Some(tabs) = result.get("tabs").and_then(|v| v.as_array()) {
                for tab in tabs {
                    let id = tab.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                    let title = tab.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    let url = tab.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let active = tab.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
                    let marker = if active { " *" } else { "" };
                    println!("[{id}]{marker} {title}");
                    println!("  {url}");
                }
                println!("\n{} tab(s)", tabs.len());
            }
        }
        "read" => {
            if let Some(title) = result.get("title").and_then(|v| v.as_str()) {
                println!("--- {} ---", title);
            }
            if let Some(url) = result.get("url").and_then(|v| v.as_str()) {
                println!("{url}\n");
            }
            if let Some(text) = result.get("text").and_then(|v| v.as_str()) {
                println!("{text}");
            }
        }
        "screenshot" => {
            if let Some(data_url) = result.get("screenshot").and_then(|v| v.as_str()) {
                if let Some(b64) = data_url.strip_prefix("data:image/jpeg;base64,") {
                    if let Ok(bytes) =
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
                    {
                        let path =
                            format!("/tmp/sidekar-ext-screenshot-{}.jpg", rand::random::<u32>());
                        if std::fs::write(&path, &bytes).is_ok() {
                            println!("Screenshot saved: {path}");
                            return;
                        }
                    }
                }
                println!("Screenshot captured ({} bytes)", data_url.len());
            }
        }
        "axtree" => {
            if let Some(title) = result.get("title").and_then(|v| v.as_str()) {
                println!("--- {} ---", title);
            }
            if let Some(elements) = result.get("elements").and_then(|v| v.as_array()) {
                for el in elements {
                    let r = el.get("ref").and_then(|v| v.as_u64()).unwrap_or(0);
                    let role = el.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    let name = el.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    println!("[{r}] {role}: {name}");
                }
                println!("\n{} interactive element(s)", elements.len());
            }
        }
        "navigate" => {
            if let Some(title) = result.get("title").and_then(|v| v.as_str()) {
                println!("--- {} ---", title);
            }
            if let Some(url) = result.get("url").and_then(|v| v.as_str()) {
                println!("{url}");
            }
        }
        "newtab" => {
            let id = result.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
            let title = result.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let url = result.get("url").and_then(|v| v.as_str()).unwrap_or("");
            println!("Opened tab [{id}] {title}");
            println!("  {url}");
        }
        "close" => {
            let id = result.get("tabId").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("Closed tab [{id}]");
        }
        "paste" => {
            let mode = result
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let len = result.get("length").and_then(|v| v.as_u64()).unwrap_or(0);
            let verified = result
                .get("verified")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if verified {
                println!("Pasted {len} chars via {mode}");
            } else {
                println!("Paste attempted via {mode} ({len} chars, not verified)");
            }
            if let Some(err) = result.get("clipboard_error").and_then(|v| v.as_str()) {
                println!("Clipboard write warning: {err}");
            }
            if result
                .get("plain_text_fallback")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                println!("Used plain-text fallback for HTML content");
            }
            if let Some(from) = result.get("fallback_from").and_then(|v| v.as_str()) {
                if from != "none" {
                    println!("Fallback source: {from}");
                }
            }
            if let Some(err) = result.get("debugger_error").and_then(|v| v.as_str()) {
                println!("Debugger warning: {err}");
            }
            if let Some(err) = result.get("insert_text_error").and_then(|v| v.as_str()) {
                println!("InsertText warning: {err}");
            }
        }
        "setvalue" => {
            let mode = result
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let len = result.get("length").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("Set value via {mode} ({len} chars)");
        }
        "evalpage" => {
            if let Some(value) = result.get("result") {
                if value.is_string() {
                    println!("{}", value.as_str().unwrap_or_default());
                } else {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(value).unwrap_or_default()
                    );
                }
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(result).unwrap_or_default()
                );
            }
        }
        _ => {
            println!(
                "{}",
                serde_json::to_string_pretty(result).unwrap_or_default()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Native Messaging Host
// ---------------------------------------------------------------------------

const NATIVE_HOST_NAME: &str = "dev.sidekar";

/// Run as a native messaging host. Reads JSON messages from stdin (length-prefixed),
/// processes commands, and writes responses to stdout (length-prefixed).
pub fn run_native_host() -> Result<()> {
    use std::io::{BufRead, Read, Write};
    use std::os::unix::net::UnixStream as StdUnixStream;
    use std::sync::{Arc, Mutex as StdMutex};

    fn write_native_message(
        stdout: &Arc<StdMutex<std::io::Stdout>>,
        value: &Value,
    ) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        let len = (bytes.len() as u32).to_le_bytes();
        let mut stdout = stdout.lock().map_err(|_| anyhow!("stdout lock poisoned"))?;
        stdout.write_all(&len)?;
        stdout.write_all(&bytes)?;
        stdout.flush()?;
        Ok(())
    }

    let mut stdin = std::io::stdin().lock();
    let stdout = Arc::new(StdMutex::new(std::io::stdout()));
    let mut daemon_writer: Option<StdUnixStream> = None;

    loop {
        let mut len_buf = [0u8; 4];
        if stdin.read_exact(&mut len_buf).is_err() {
            break;
        }
        let len = u32::from_le_bytes(len_buf) as usize;
        if len == 0 || len > 1024 * 1024 {
            break;
        }
        let mut msg_buf = vec![0u8; len];
        if stdin.read_exact(&mut msg_buf).is_err() {
            break;
        }

        let msg = match serde_json::from_slice::<Value>(&msg_buf) {
            Ok(msg) => msg,
            Err(e) => {
                let _ = write_native_message(&stdout, &json!({"error": format!("Invalid JSON: {e}")}));
                continue;
            }
        };

        let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if msg_type == "bridge_register" {
            let cli_logged_in = crate::auth::auth_token().is_some();
            if !cli_logged_in {
                let _ = write_native_message(
                    &stdout,
                    &json!({
                        "type": "auth_fail",
                        "reason": "Run `sidekar login`",
                        "cli_logged_in": false
                    }),
                );
                continue;
            }

            let ext_token = msg.get("token").and_then(|v| v.as_str()).unwrap_or("").trim();
            if ext_token.is_empty() {
                let _ = write_native_message(
                    &stdout,
                    &json!({
                        "type": "auth_fail",
                        "reason": "No token provided — log in from the extension popup",
                        "cli_logged_in": true
                    }),
                );
                continue;
            }

            let user_id = match verify_ext_token_sync(ext_token) {
                Ok(user_id) => user_id,
                Err(e) => {
                    let _ = write_native_message(
                        &stdout,
                        &json!({
                            "type": "auth_fail",
                            "reason": format!("{e:#}"),
                            "cli_logged_in": true
                        }),
                    );
                    continue;
                }
            };

            if let Err(e) = crate::daemon::ensure_running() {
                let _ = write_native_message(
                    &stdout,
                    &json!({
                        "type": "auth_fail",
                        "reason": format!("Failed to start daemon: {e:#}"),
                        "cli_logged_in": true
                    }),
                );
                continue;
            }

            let mut stream = match StdUnixStream::connect(crate::daemon::socket_path()) {
                Ok(stream) => stream,
                Err(e) => {
                    let _ = write_native_message(
                        &stdout,
                        &json!({
                            "type": "auth_fail",
                            "reason": format!("Cannot connect to daemon: {e}"),
                            "cli_logged_in": true
                        }),
                    );
                    continue;
                }
            };

            let register = json!({
                "type": "ext_bridge_register",
                "user_id": user_id,
                "version": msg.get("version").cloned().unwrap_or(json!("?"))
            });
            let mut line = serde_json::to_string(&register)?;
            line.push('\n');
            if stream.write_all(line.as_bytes()).is_err() || stream.flush().is_err() {
                let _ = write_native_message(
                    &stdout,
                    &json!({
                        "type": "auth_fail",
                        "reason": "Failed to register extension bridge with daemon",
                        "cli_logged_in": true
                    }),
                );
                continue;
            }

            let reader_stream = match stream.try_clone() {
                Ok(v) => v,
                Err(e) => {
                    let _ = write_native_message(
                        &stdout,
                        &json!({
                            "type": "auth_fail",
                            "reason": format!("Failed to clone daemon stream: {e}"),
                            "cli_logged_in": true
                        }),
                    );
                    continue;
                }
            };

            let mut ack = String::new();
            let mut ack_reader = std::io::BufReader::new(reader_stream);
            match ack_reader.read_line(&mut ack) {
                Ok(0) | Err(_) => {
                    let _ = write_native_message(
                        &stdout,
                        &json!({
                            "type": "auth_fail",
                            "reason": "Daemon did not acknowledge extension bridge registration",
                            "cli_logged_in": true
                        }),
                    );
                    continue;
                }
                Ok(_) => {}
            }
            if serde_json::from_str::<Value>(ack.trim()).is_err() {
                let _ = write_native_message(
                    &stdout,
                    &json!({
                        "type": "auth_fail",
                        "reason": "Daemon returned invalid extension bridge registration ack",
                        "cli_logged_in": true
                    }),
                );
                continue;
            }

            let bridge_reader = ack_reader.into_inner();
            let bridge_stdout = stdout.clone();
            std::thread::spawn(move || {
                let mut reader = std::io::BufReader::new(bridge_reader);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(_) => {
                            if let Ok(value) = serde_json::from_str::<Value>(line.trim()) {
                                let _ = write_native_message(&bridge_stdout, &value);
                            }
                        }
                        Err(_) => break,
                    }
                }
                std::process::exit(0);
            });

            daemon_writer = Some(stream);
            let _ = write_native_message(
                &stdout,
                &json!({"type": "auth_ok", "cli_logged_in": true}),
            );
            continue;
        }

        if msg_type == "ping" {
            let _ = write_native_message(&stdout, &json!({"pong": true}));
            continue;
        }

        if let Some(stream) = daemon_writer.as_mut() {
            let mut line = serde_json::to_string(&msg)?;
            line.push('\n');
            if stream.write_all(line.as_bytes()).is_err() || stream.flush().is_err() {
                break;
            }
        } else {
            let _ = write_native_message(
                &stdout,
                &json!({"error": "Extension bridge not registered"}),
            );
        }
    }

    Ok(())
}

fn write_native_host_manifests(
    manifest_dirs: &[PathBuf],
    extension_id: Option<&str>,
    verbose: bool,
) -> Result<()> {
    let exe_path = std::env::current_exe().context("Cannot determine sidekar executable path")?;

    // Create a wrapper script that calls sidekar with the native-messaging-host command
    let wrapper_path = exe_path
        .parent()
        .unwrap_or(std::path::Path::new("/usr/local/bin"))
        .join("sidekar-native-host");

    let wrapper_script = format!(
        "#!/bin/bash\nexec \"{}\" native-messaging-host \"$@\"\n",
        exe_path.display()
    );
    std::fs::write(&wrapper_path, &wrapper_script).with_context(|| {
        format!(
            "Failed to write wrapper script to {}",
            wrapper_path.display()
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper_path, std::fs::Permissions::from_mode(0o755))?;
    }

    let manifest_paths: Vec<PathBuf> = manifest_dirs
        .iter()
        .map(|dir| dir.join(format!("{NATIVE_HOST_NAME}.json")))
        .collect();
    let allowed_origins = native_host_allowed_origins(&manifest_paths, extension_id)?;
    let manifest = json!({
        "name": NATIVE_HOST_NAME,
        "description": "Sidekar native messaging host",
        "path": wrapper_path.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": allowed_origins
    });
    let manifest_json = serde_json::to_string_pretty(&manifest)?;

    for manifest_dir in manifest_dirs {
        std::fs::create_dir_all(manifest_dir).with_context(|| {
            format!(
                "Failed to create NativeMessagingHosts directory {}",
                manifest_dir.display()
            )
        })?;
    }
    for manifest_path in &manifest_paths {
        std::fs::write(manifest_path, &manifest_json)
            .with_context(|| format!("Failed to write {}", manifest_path.display()))?;
    }

    if verbose {
        println!("Installed native messaging host manifests:");
        for manifest_path in &manifest_paths {
            println!("  {}", manifest_path.display());
        }
        println!();
        println!("Manifest contents:");
        println!("{manifest_json}");
    }

    Ok(())
}

/// Install the native messaging host manifest for Chrome.
fn install_native_host_impl(extension_id: Option<&str>, verbose: bool) -> Result<()> {
    let manifest_dirs = native_host_manifest_dirs()?;
    write_native_host_manifests(&manifest_dirs, extension_id, verbose)
}

pub fn install_native_host(extension_id: Option<&str>) -> Result<()> {
    install_native_host_impl(extension_id, true)
}

pub(crate) fn install_native_host_quiet(extension_id: Option<&str>) -> Result<()> {
    install_native_host_impl(extension_id, false)
}

pub(crate) fn install_native_host_for_profile_dir(profile_dir: &std::path::Path) -> Result<()> {
    let manifest_dir = profile_dir.join("NativeMessagingHosts");
    write_native_host_manifests(&[manifest_dir], None, false)
}
