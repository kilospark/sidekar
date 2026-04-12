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

use crate::message::epoch_secs;

use dirs;
use zip::ZipArchive;

const DEFAULT_API_URL: &str = "https://sidekar.dev";

pub const EXTENSION_ZIP: &[u8] = include_bytes!("../assets/extension.zip");

mod cli;
mod verify;

pub use cli::send_cli_command;
pub use verify::{VerifyResult, is_ext_available, verify_ext_token};

/// Paste / cli_exec can exceed 30s (CDP attach, Google Docs focus path).
const TIMEOUT_SECS: u64 = 180;

// ---------------------------------------------------------------------------
// Shared state between server and extension connections
// ---------------------------------------------------------------------------

/// A single extension bridge connection (one per Chrome profile).
pub struct ExtConnection {
    pub bridge_tx: mpsc::UnboundedSender<String>,
    pub pending: HashMap<String, oneshot::Sender<Value>>,
    /// Extension-initiated `cli_exec` work running in the daemon (inserttext / keyboard).
    pub cli_exec_inflight: u32,
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
    if let Some(u) = url
        && !u.is_empty()
    {
        message.push_str(&format!("\nURL: {u}"));
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
            cli_exec_inflight: 0,
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

pub async fn cli_exec_begin(state: &SharedExtState, connection_id: u64) {
    let mut s = state.lock().await;
    if let Some(conn) = s.connections.get_mut(&connection_id) {
        conn.cli_exec_inflight = conn.cli_exec_inflight.saturating_add(1);
    }
}

pub async fn cli_exec_end(state: &SharedExtState, connection_id: u64) {
    let mut s = state.lock().await;
    if let Some(conn) = s.connections.get_mut(&connection_id) {
        conn.cli_exec_inflight = conn.cli_exec_inflight.saturating_sub(1);
        conn.last_contact = epoch_secs();
    }
}

/// Route an inbound response (by id) to the correct pending oneshot.
pub async fn resolve_pending(state: &SharedExtState, connection_id: u64, val: Value) {
    if let Some(id) = val.get("id").and_then(|v| v.as_str()) {
        let mut s = state.lock().await;
        if let Some(conn) = s.connections.get_mut(&connection_id)
            && let Some(tx) = conn.pending.remove(id)
        {
            let _ = tx.send(val);
        }
    }
}

/// Send a command to the extension via the shared state.
/// Used by daemon to forward ext commands from unix socket.
pub struct RoutedCommandResult {
    pub response: Value,
    pub conn_id: u64,
    pub profile: String,
}

pub async fn forward_command(
    state: &SharedExtState,
    command: Value,
    agent_id: Option<String>,
    target_conn: Option<u64>,
    target_profile: Option<String>,
) -> Result<RoutedCommandResult> {
    send_command(
        state,
        command,
        agent_id.as_deref(),
        target_conn,
        target_profile.as_deref(),
    )
    .await
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
/// Priority: target_conn > target_profile > single available connection.
async fn send_command(
    state: &SharedState,
    command: Value,
    _agent_id: Option<&str>,
    target_conn: Option<u64>,
    target_profile: Option<&str>,
) -> Result<RoutedCommandResult> {
    let id = format!("{:08x}", rand::random::<u32>());
    let mut msg = command;
    msg.as_object_mut().unwrap().insert("id".into(), json!(id));

    let (tx, rx) = oneshot::channel();
    let (conn_id, profile) = {
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
        } else if s.connections.len() == 1 {
            *s.connections.keys().next().unwrap()
        } else {
            bail!(
                "Multiple extension connections are available. Rerun with `--conn` or `--profile`."
            );
        };

        let conn = s.connections.get_mut(&conn_id).unwrap();
        let profile = conn.profile.clone();
        conn.pending.insert(id.clone(), tx);
        let text = serde_json::to_string(&msg)?;
        if conn.bridge_tx.send(text).is_err() {
            conn.pending.remove(&id);
            bail!("Failed to send to extension bridge");
        }
        (conn_id, profile)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(TIMEOUT_SECS), rx).await {
        Ok(Ok(val)) => Ok(RoutedCommandResult {
            response: val,
            conn_id,
            profile,
        }),
        Ok(Err(_)) => bail!("Extension response channel closed"),
        Err(_) => {
            let mut s = state.lock().await;
            if let Some(conn) = s.connections.get_mut(&conn_id) {
                conn.pending.remove(&id);
            }
            bail!("Extension command timed out ({TIMEOUT_SECS}s)")
        }
    }
}
