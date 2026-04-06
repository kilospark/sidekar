//! Extension bridge for Chrome extension communication.
//!
//! The Chrome extension connects via localhost WebSocket, and the daemon
//! routes `sidekar ext <command>` requests to the connected extension bridge.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::auth;
use crate::message::epoch_secs;

use dirs;
use zip::ZipArchive;

const DEFAULT_API_URL: &str = "https://sidekar.dev";

pub const EXTENSION_ZIP: &[u8] = include_bytes!("../assets/extension.zip");

/// Paste / cli_exec can exceed 30s (CDP attach, Google Docs focus path).
const TIMEOUT_SECS: u64 = 180;

/// Token verification cache keyed by ext_token prefix (first 16 chars).
/// Avoids network call on every extension reconnect.
struct CacheEntry {
    user_id: String,
    expires_at: u64,
}

static TOKEN_CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, CacheEntry>>> =
    std::sync::OnceLock::new();

fn token_cache_key(ext_token: &str) -> String {
    ext_token.chars().take(16).collect()
}

fn get_cached_user_id(ext_token: &str) -> Option<String> {
    let map = TOKEN_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let map = map.lock().ok()?;
    let entry = map.get(&token_cache_key(ext_token))?;
    if entry.expires_at > epoch_secs() {
        Some(entry.user_id.clone())
    } else {
        None
    }
}

fn set_cached_user_id(ext_token: &str, user_id: String) {
    let map = TOKEN_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Ok(mut map) = map.lock() {
        // Evict expired entries
        let now = epoch_secs();
        map.retain(|_, v| v.expires_at > now);
        map.insert(token_cache_key(ext_token), CacheEntry {
            user_id,
            expires_at: now + 300,
        });
    }
}

// ---------------------------------------------------------------------------
// Shared state between server and extension connections
// ---------------------------------------------------------------------------

/// A single extension bridge connection (one per Chrome profile).
pub struct ExtConnection {
    pub bridge_tx: mpsc::UnboundedSender<String>,
    pub pending: HashMap<String, oneshot::Sender<Value>>,
    pub verified_user_id: String,
    pub last_contact: u64,
    pub owner_agent_id: Option<String>,
    pub profile: String,
    pub browser: String,
}

/// A registered DOM watcher. Watch events are delivered via broker to `deliver_to`.
pub struct WatchRecord {
    pub watch_id: String,
    pub selector: String,
    pub deliver_to: String,
    pub conn_id: u64,
    pub profile: String,
    pub created_at: u64,
}

pub struct ExtState {
    pub connections: HashMap<u64, ExtConnection>,
    pub next_connection_id: u64,
    pub watches: HashMap<String, WatchRecord>,
}

impl Default for ExtState {
    fn default() -> Self {
        Self {
            connections: HashMap::new(),
            next_connection_id: 1,
            watches: HashMap::new(),
        }
    }
}

pub type SharedExtState = Arc<Mutex<ExtState>>;

fn ext_api_base() -> String {
    std::env::var("SIDEKAR_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string())
}

/// Verification outcome with structured error classification.
pub enum VerifyResult {
    /// Token verified, user_id returned.
    Ok(String),
    /// Token is definitively invalid — extension should clear it.
    InvalidToken(String),
    /// Transient/network error — extension should retry, NOT clear token.
    TransientError(String),
}

pub fn verify_ext_token(ext_token: &str) -> VerifyResult {
    // Try cache first
    if let Some(cached_user_id) = get_cached_user_id(ext_token) {
        return VerifyResult::Ok(cached_user_id);
    }

    let device_token = match auth::auth_token() {
        Some(t) => t,
        None => return VerifyResult::TransientError("CLI not logged in. Run: sidekar device login".into()),
    };

    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => return VerifyResult::TransientError(format!("HTTP client error: {e}")),
    };

    let url = format!("{}/api/auth/device?action=ext-verify", ext_api_base());
    let resp = match client
        .post(&url)
        .header("Authorization", format!("Bearer {}", device_token))
        .json(&json!({ "ext_token": ext_token }))
        .send()
    {
        Ok(r) => r,
        Err(e) => return VerifyResult::TransientError(format!("Cannot reach sidekar.dev: {e}")),
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        // 401 with "invalid ext token" or "invalid device token" = definitive
        if status.as_u16() == 401 {
            return VerifyResult::InvalidToken(format!("Token rejected by server ({status})"));
        }
        // Other HTTP errors are transient
        return VerifyResult::TransientError(format!("Server error: HTTP {status} — {body}"));
    }

    let data: Value = match resp.json() {
        Ok(d) => d,
        Err(e) => return VerifyResult::TransientError(format!("Invalid response: {e}")),
    };

    let matched = data.get("match").and_then(|v| v.as_bool()).unwrap_or(false);
    if !matched {
        return VerifyResult::InvalidToken("Extension token and CLI token belong to different users".into());
    }

    let user_id = match data.get("user_id").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return VerifyResult::TransientError("No user_id in verification response".into()),
    };

    // Cache the result
    set_cached_user_id(ext_token, user_id.clone());

    VerifyResult::Ok(user_id)
}

type SharedState = Arc<Mutex<ExtState>>;

// ---------------------------------------------------------------------------
// Ext bridge for daemon
// ---------------------------------------------------------------------------

/// Disconnect a specific bridge connection by id.
pub async fn disconnect_bridge_by_id(state: &SharedExtState, connection_id: u64) {
    disconnect_bridge(state, connection_id).await;
}

async fn disconnect_bridge(state: &SharedState, connection_id: u64) {
    let pending = {
        let mut s = state.lock().await;
        // Remove watches owned by this connection
        s.watches.retain(|_, w| w.conn_id != connection_id);
        match s.connections.remove(&connection_id) {
            Some(conn) => conn.pending,
            None => return,
        }
    };
    for (_id, tx) in pending {
        let _ = tx.send(json!({"error": "Extension disconnected"}));
    }
}

/// Register a watch record after the extension confirms setup.
pub async fn register_watch(
    state: &SharedExtState,
    watch_id: String,
    selector: String,
    deliver_to: String,
    conn_id: u64,
    profile: String,
) {
    let mut s = state.lock().await;
    s.watches.insert(
        watch_id.clone(),
        WatchRecord {
            watch_id,
            selector,
            deliver_to,
            conn_id,
            profile,
            created_at: epoch_secs(),
        },
    );
}

/// Remove a watch record (called on unwatch or connection drop).
pub async fn remove_watch(state: &SharedExtState, watch_id: &str) -> Option<WatchRecord> {
    let mut s = state.lock().await;
    s.watches.remove(watch_id)
}

/// Look up delivery target for a watch event.
pub async fn find_watch_target(state: &SharedExtState, watch_id: &str) -> Option<(String, String)> {
    let s = state.lock().await;
    s.watches
        .get(watch_id)
        .map(|w| (w.deliver_to.clone(), w.selector.clone()))
}

/// Get all active watches for status display.
pub async fn list_watches(state: &SharedExtState) -> Vec<Value> {
    let s = state.lock().await;
    s.watches
        .values()
        .map(|w| {
            json!({
                "watchId": w.watch_id,
                "selector": w.selector,
                "deliverTo": w.deliver_to,
                "profile": w.profile,
                "createdAt": w.created_at,
            })
        })
        .collect()
}

/// Deliver a watch event via the broker to the registered agent.
pub async fn deliver_watch_event(
    state: &SharedExtState,
    watch_id: &str,
    current: &str,
    previous: &str,
    url: Option<&str>,
) -> Result<()> {
    let (deliver_to, selector) = match find_watch_target(state, watch_id).await {
        Some(v) => v,
        None => return Ok(()), // watch was removed; drop event
    };

    let mut message = format!("Element changed on {selector}");
    if let Some(u) = url {
        if !u.is_empty() {
            message.push_str(&format!("\nURL: {u}"));
        }
    }
    if !previous.is_empty() {
        let prev_trim = if previous.len() > 500 {
            &previous[..500]
        } else {
            previous
        };
        message.push_str(&format!("\nBefore: {prev_trim}"));
    }
    if !current.is_empty() {
        let cur_trim = if current.len() > 500 {
            &current[..500]
        } else {
            current
        };
        message.push_str(&format!("\nAfter: {cur_trim}"));
    }

    let formatted = format!("[from sidekar-ext-watch]: {message}");
    crate::broker::enqueue_message("sidekar-ext-watch", &deliver_to, &formatted)?;
    Ok(())
}

/// Register a new bridge connection and return the connection_id and bridge_rx.
/// Used by the WebSocket path in daemon.rs.
pub async fn register_bridge_ws(
    state: &SharedExtState,
    user_id: String,
    agent_id: Option<String>,
    browser: String,
) -> (u64, mpsc::UnboundedReceiver<String>, String) {
    let now = epoch_secs();
    let (bridge_tx, bridge_rx) = mpsc::unbounded_channel::<String>();
    let mut s = state.lock().await;
    let cid = s.next_connection_id;
    s.next_connection_id = cid.wrapping_add(1);
    let profile = browser.to_lowercase();
    s.connections.insert(
        cid,
        ExtConnection {
            bridge_tx,
            pending: HashMap::new(),
            verified_user_id: user_id,
            last_contact: now,
            owner_agent_id: agent_id,
            profile: profile.clone(),
            browser,
        },
    );
    (cid, bridge_rx, profile)
}

/// Update last_contact for a connection.
pub async fn touch_connection(state: &SharedExtState, connection_id: u64) {
    let mut s = state.lock().await;
    if let Some(conn) = s.connections.get_mut(&connection_id) {
        conn.last_contact = epoch_secs();
    }
}

/// Route an inbound response (by id) to the correct pending oneshot.
pub async fn resolve_pending(state: &SharedExtState, connection_id: u64, val: Value) {
    if let Some(id) = val.get("id").and_then(|v| v.as_str()) {
        let mut s = state.lock().await;
        if let Some(conn) = s.connections.get_mut(&connection_id) {
            if let Some(tx) = conn.pending.remove(id) {
                let _ = tx.send(val);
            }
        }
    }
}

/// Send a command to the extension via the shared state.
/// Used by daemon to forward ext commands from unix socket.
pub async fn forward_command(
    state: &SharedExtState,
    command: Value,
    agent_id: Option<String>,
    target_conn: Option<u64>,
    target_profile: Option<String>,
) -> Value {
    match send_command(
        state,
        command,
        agent_id.as_deref(),
        target_conn,
        target_profile.as_deref(),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => json!({"error": e.to_string()}),
    }
}

/// Get extension connection status.
pub async fn get_status(state: &SharedExtState) -> Value {
    let s = state.lock().await;
    let count = s.connections.len();
    let connected = count > 0;
    let details: Vec<Value> = s
        .connections
        .iter()
        .map(|(id, c)| {
            json!({
                "id": id,
                "profile": c.profile,
                "browser": c.browser,
                "user_id": c.verified_user_id,
                "owner": c.owner_agent_id,
            })
        })
        .collect();
    json!({
        "connected": connected,
        "authenticated": connected,
        "connections": details,
    })
}

/// Pick a connection and send a command.
/// Priority: target_conn > target_profile > agent ownership > first available.
async fn send_command(
    state: &SharedState,
    command: Value,
    agent_id: Option<&str>,
    target_conn: Option<u64>,
    target_profile: Option<&str>,
) -> Result<Value> {
    let id = format!("{:08x}", rand::random::<u32>());
    let mut msg = command;
    msg.as_object_mut().unwrap().insert("id".into(), json!(id));

    let (tx, rx) = oneshot::channel();

    {
        let mut s = state.lock().await;
        if s.connections.is_empty() {
            bail!("Extension not connected. Is Chrome running with the Sidekar extension?");
        }

        let conn_id = if let Some(cid) = target_conn {
            // Explicit connection ID
            if !s.connections.contains_key(&cid) {
                bail!("Connection {cid} not found. Use `sidekar ext status` to list connections.");
            }
            cid
        } else if let Some(profile) = target_profile {
            // Match by profile name (case-insensitive substring)
            let lp = profile.to_lowercase();
            let found = s
                .connections
                .iter()
                .find(|(_, c)| c.profile.to_lowercase().contains(&lp))
                .map(|(id, _)| *id);
            match found {
                Some(cid) => cid,
                None => bail!(
                    "No connection matching profile '{profile}'. Use `sidekar ext status` to list."
                ),
            }
        } else if let Some(req_agent) = agent_id {
            // Find connection owned by this agent
            let owned = s
                .connections
                .iter()
                .find(|(_, c)| c.owner_agent_id.as_deref() == Some(req_agent))
                .map(|(id, _)| *id);
            if let Some(cid) = owned {
                cid
            } else {
                let unowned = s
                    .connections
                    .iter()
                    .find(|(_, c)| c.owner_agent_id.is_none())
                    .map(|(id, _)| *id);
                match unowned {
                    Some(cid) => {
                        s.connections.get_mut(&cid).unwrap().owner_agent_id =
                            Some(req_agent.to_string());
                        cid
                    }
                    None => *s.connections.keys().next().unwrap(),
                }
            }
        } else {
            *s.connections.keys().next().unwrap()
        };

        let conn = s.connections.get_mut(&conn_id).unwrap();
        conn.pending.insert(id.clone(), tx);
        let text = serde_json::to_string(&msg)?;
        if conn.bridge_tx.send(text).is_err() {
            conn.pending.remove(&id);
            bail!("Failed to send to extension bridge");
        }
    }

    match tokio::time::timeout(std::time::Duration::from_secs(TIMEOUT_SECS), rx).await {
        Ok(Ok(val)) => Ok(val),
        Ok(Err(_)) => bail!("Extension response channel closed"),
        Err(_) => {
            let mut s = state.lock().await;
            for conn in s.connections.values_mut() {
                conn.pending.remove(&id);
            }
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
    if command == "dev-extract" {
        return extract_extension();
    }

    // Parse --conn and --profile from args
    let mut filtered_args = Vec::new();
    let mut target_conn: Option<u64> = None;
    let mut target_profile: Option<String> = None;
    let mut skip_next = false;
    for (i, arg) in args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--conn" {
            if let Some(val) = args.get(i + 1) {
                target_conn = Some(
                    val.parse()
                        .context("--conn requires a numeric connection ID")?,
                );
                skip_next = true;
            }
        } else if arg == "--profile" {
            if let Some(val) = args.get(i + 1) {
                target_profile = Some(val.clone());
                skip_next = true;
            }
        } else {
            filtered_args.push(arg.clone());
        }
    }

    let msg = build_command(command, &filtered_args, default_tab)?;
    crate::daemon::ensure_running()?;

    let agent_id = std::env::var("SIDEKAR_AGENT_ID").ok();
    let mut cmd_json = json!({
        "type": "ext",
        "command": msg,
    });
    if let Some(ref aid) = agent_id {
        cmd_json["agent_id"] = json!(aid);
    }
    if let Some(cid) = target_conn {
        cmd_json["conn_id"] = json!(cid);
    }
    if let Some(ref p) = target_profile {
        cmd_json["profile"] = json!(p);
    }

    // For `watch`, resolve the caller's broker agent so the daemon can deliver
    // watch events back via the bus.
    if command == "watch" {
        if let Some(agent) = crate::bus::inherit_pty_registration() {
            cmd_json["deliver_to"] = json!(agent.name);
        } else {
            bail!(
                "sidekar ext watch must be run inside a sidekar-wrapped agent session \
                 (sidekar claude, sidekar codex, etc.) so events can be delivered to the bus."
            );
        }
    }

    let result = crate::daemon::send_command(&cmd_json)?;

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
    let conns = status.get("connections").and_then(|v| v.as_array());

    match conns {
        Some(list) if !list.is_empty() => {
            println!("{} connection(s):", list.len());
            for c in list {
                let id = c.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                let browser = c.get("browser").and_then(|v| v.as_str()).unwrap_or("?");
                let owner = c.get("owner").and_then(|v| v.as_str());
                print!("  [{id}] {browser}");
                if let Some(o) = owner {
                    print!(" (owner: {o})");
                }
                println!();
            }
        }
        _ => {
            println!("No extension connections");
        }
    }
    Ok(())
}

fn extract_extension() -> Result<()> {
    let home = dirs::home_dir().ok_or(anyhow!("No home directory found"))?;
    let target_dir = home.join(".sidekar/extension");

    fs::create_dir_all(&target_dir).context("Failed to create .sidekar directory")?;

    let reader = Cursor::new(EXTENSION_ZIP);
    let mut archive = ZipArchive::new(reader).context("Failed to read embedded ZIP")?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).context("Failed to access ZIP entry")?;
        let outpath = target_dir.join(file.name());

        if file.name().ends_with('/') {
            fs::create_dir_all(&outpath).context("Failed to create directory in extraction")?;
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent).context("Failed to create parent directory")?;
            }
            let mut outfile = fs::File::create(&outpath).context("Failed to create output file")?;
            std::io::copy(&mut file, &mut outfile).context("Failed to copy file contents")?;
        }

        // Set permissions
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = file.unix_mode() {
                fs::set_permissions(&outpath, std::fs::Permissions::from_mode(mode))
                    .context("Failed to set file permissions")?;
            }
        }
    }

    println!(
        "Chrome extension extracted/updated to {}",
        target_dir.display()
    );
    println!(
        "To load: Chrome > Extensions > Enable Developer mode > Load unpacked > Select {}",
        target_dir.display()
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
            if text.as_deref().unwrap_or("").is_empty() && html.as_deref().unwrap_or("").is_empty()
            {
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
        "set-value" => {
            if args.len() < 2 {
                bail!("Usage: sidekar ext set-value <selector> <text>");
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
        "ax-tree" => {
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
        "eval-page" => {
            let code = args.join(" ");
            if code.is_empty() {
                bail!("Usage: sidekar ext eval-page <javascript>");
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
        "new-tab" => {
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
        "history" => {
            let query = args.join(" ");
            let max_results = 25u64; // default
            Ok(json!({"command": "history", "query": query, "maxResults": max_results}))
        }
        "watch" => {
            let selector = args
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("Usage: sidekar ext watch <selector> [--tab <id>]"))?;
            let mut cmd = json!({"command": "watch", "selector": selector});
            if let Some(id) = default_tab {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "unwatch" => {
            let mut cmd = json!({"command": "unwatch"});
            if let Some(watch_id) = args.first() {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("watchId".into(), json!(watch_id));
            }
            Ok(cmd)
        }
        "watchers" => Ok(json!({"command": "watchers"})),
        "context" => Ok(json!({"command": "context"})),
        _ => bail!(
            "Unknown ext command: {command}\nAvailable: tabs, read, screenshot, click, type, paste, set-value, ax-tree, eval, eval-page, navigate, new-tab, close, scroll, history, watch, unwatch, watchers, context, status, stop"
        ),
    }
}

/// Format a JS timestamp (milliseconds since epoch) as a relative time string.
fn format_time_ago(ts_ms: f64) -> String {
    if ts_ms <= 0.0 {
        return "unknown".to_string();
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as f64;
    let diff_secs = ((now_ms - ts_ms) / 1000.0).max(0.0) as u64;
    if diff_secs < 60 {
        format!("{diff_secs}s ago")
    } else if diff_secs < 3600 {
        format!("{}m ago", diff_secs / 60)
    } else if diff_secs < 86400 {
        format!("{}h ago", diff_secs / 3600)
    } else {
        format!("{}d ago", diff_secs / 86400)
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
        "ax-tree" => {
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
        "new-tab" => {
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
        "set-value" => {
            let mode = result
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let len = result.get("length").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("Set value via {mode} ({len} chars)");
        }
        "eval-page" => {
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
        "history" => {
            if let Some(entries) = result.get("entries").and_then(|v| v.as_array()) {
                for entry in entries {
                    let title = entry.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    let url = entry.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let visits = entry
                        .get("visitCount")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let ts = entry
                        .get("lastVisitTime")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let ago = format_time_ago(ts);
                    println!("{title}");
                    println!("  {url}");
                    println!("  {ago} | {visits} visit(s)");
                    println!();
                }
                println!("{} result(s)", entries.len());
            }
        }
        "watch" => {
            if let Some(watch_id) = result.get("watchId").and_then(|v| v.as_str()) {
                let selector = result
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                println!("Watching: {selector}");
                println!("Watch ID: {watch_id}");
                if let Some(deliver) = result.get("deliverTo").and_then(|v| v.as_str()) {
                    println!("Events will be delivered to: {deliver}");
                }
                if let Some(state) = result.get("initialState").and_then(|v| v.as_str()) {
                    if !state.is_empty() {
                        let preview = if state.len() > 200 {
                            &state[..200]
                        } else {
                            state
                        };
                        println!("Current state: {preview}");
                    }
                }
            }
        }
        "unwatch" => {
            if let Some(count) = result.get("count").and_then(|v| v.as_u64()) {
                println!("Removed {count} watcher(s)");
            } else if let Some(wid) = result.get("watchId").and_then(|v| v.as_str()) {
                println!("Removed watcher: {wid}");
            }
        }
        "watchers" => {
            if let Some(watchers) = result.get("watchers").and_then(|v| v.as_array()) {
                if watchers.is_empty() {
                    println!("No active watchers");
                } else {
                    for w in watchers {
                        let wid = w.get("watchId").and_then(|v| v.as_str()).unwrap_or("?");
                        let sel = w.get("selector").and_then(|v| v.as_str()).unwrap_or("?");
                        let tab = w.get("tabId").and_then(|v| v.as_u64()).unwrap_or(0);
                        println!("[{wid}] tab:{tab} {sel}");
                    }
                    println!("\n{} watcher(s)", watchers.len());
                }
            }
        }
        "context" => {
            // Active tab
            if let Some(tab) = result.get("active_tab") {
                let title = tab.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let url = tab.get("url").and_then(|v| v.as_str()).unwrap_or("");
                println!("Active: {title}");
                println!("  {url}\n");
            }

            // Windows + tabs
            if let Some(windows) = result.get("windows").and_then(|v| v.as_object()) {
                let tab_count = result
                    .get("tab_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let win_count = result
                    .get("window_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                println!("{tab_count} tab(s) across {win_count} window(s):");
                for (wid, tabs) in windows {
                    if let Some(tabs) = tabs.as_array() {
                        println!("  Window {wid}:");
                        for t in tabs {
                            let id = t.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                            let title = t.get("title").and_then(|v| v.as_str()).unwrap_or("");
                            let active = t.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
                            let marker = if active { " *" } else { "" };
                            let short_title = if title.len() > 60 {
                                &title[..60]
                            } else {
                                title
                            };
                            println!("    [{id}]{marker} {short_title}");
                        }
                    }
                }
            }

            // Recent history
            if let Some(history) = result.get("recent_history").and_then(|v| v.as_array()) {
                if !history.is_empty() {
                    println!("\nRecent activity:");
                    for h in history.iter().take(10) {
                        let title = h.get("title").and_then(|v| v.as_str()).unwrap_or("");
                        let url = h.get("url").and_then(|v| v.as_str()).unwrap_or("");
                        let ts = h
                            .get("lastVisitTime")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                        let ago = format_time_ago(ts);
                        let short_title = if title.len() > 50 {
                            &title[..50]
                        } else {
                            title
                        };
                        // Extract domain from URL
                        let domain = url
                            .strip_prefix("https://")
                            .or_else(|| url.strip_prefix("http://"))
                            .unwrap_or(url)
                            .split('/')
                            .next()
                            .unwrap_or("");
                        println!("  {ago} | {domain} | {short_title}");
                    }
                }
            }

            // Watchers
            if let Some(watchers) = result.get("watchers").and_then(|v| v.as_array()) {
                if !watchers.is_empty() {
                    println!("\n{} active watcher(s)", watchers.len());
                }
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
