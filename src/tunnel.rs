//! Tunnel client for connecting local PTY sessions to the sidekar relay.
//!
//! Establishes a WSS connection to the relay server, registers the session,
//! and bridges PTY I/O over binary WebSocket frames. JSON text frames carry
//! the multiplex bus (`ch: "bus"`) between machines; binary frames remain PTY.
//!
//! The public API returns a `(TunnelSender, TunnelReceiver)` pair designed
//! to integrate into a `tokio::select!` event loop (see `pty.rs`).

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;

mod transport;

use transport::*;

const DEFAULT_RELAY_URL: &str = "wss://relay.sidekar.dev/tunnel";
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const RECONNECT_BASE: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);
const CHANNEL_CAPACITY: usize = 256;

// ---------------------------------------------------------------------------
// Global output tunnel — lets any module forward println-style output to
// web terminal viewers without threading a TunnelSender through every call.
// ---------------------------------------------------------------------------

static OUTPUT_TUNNEL: Mutex<Option<TunnelSender>> = Mutex::new(None);

/// Register the tunnel sender for global output forwarding.
pub fn set_output_tunnel(tx: TunnelSender) {
    if let Ok(mut guard) = OUTPUT_TUNNEL.lock() {
        *guard = Some(tx);
    }
}

/// Unregister the tunnel sender (e.g. when relay is turned off).
pub fn clear_output_tunnel() {
    if let Ok(mut guard) = OUTPUT_TUNNEL.lock() {
        *guard = None;
    }
}

/// Returns true if a tunnel sender is currently registered.
pub fn has_output_tunnel() -> bool {
    OUTPUT_TUNNEL.lock().ok().map_or(false, |g| g.is_some())
}

/// Print a line to stdout and, if a tunnel is registered, to web viewers.
/// Uses `\r\n` line endings so output is correct in raw terminal mode
/// (cfmakeraw clears OPOST, which disables the kernel's `\n` → `\r\n` translation).
pub fn tunnel_println(text: &str) {
    // Normalize embedded newlines to \r\n, then append a final \r\n
    let normalized = text.replace("\r\n", "\n").replace('\n', "\r\n");
    print!("{normalized}\r\n");
    let _ = std::io::stdout().flush();
    if let Some(ref tx) = *OUTPUT_TUNNEL.lock().unwrap_or_else(|e| e.into_inner()) {
        let mut data = normalized.into_bytes();
        data.extend_from_slice(b"\r\n");
        tx.send_data(data);
    }
}

/// Send raw bytes to the tunnel only (no stdout). No-op if no tunnel registered.
pub fn tunnel_send(data: Vec<u8>) {
    if let Some(ref tx) = *OUTPUT_TUNNEL.lock().unwrap_or_else(|e| e.into_inner()) {
        tx.send_data(data);
    }
}

fn relay_url() -> String {
    std::env::var("SIDEKAR_RELAY_URL").unwrap_or_else(|_| DEFAULT_RELAY_URL.to_string())
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Events received from the relay, delivered to the PTY event loop.
#[derive(Debug)]
pub enum TunnelEvent {
    /// Raw bytes from a browser viewer (keyboard input).
    Data(Vec<u8>),
    /// Routed bus: enqueue locally when `recipient` matches this agent name.
    BusRelay {
        recipient: String,
        sender: String,
        body: String,
        envelope: Option<crate::message::Envelope>,
    },
    /// Legacy/simple bus frame (body only) — written to PTY.
    BusPlain(String),
    /// The tunnel has disconnected (reconnect is happening in the background).
    Disconnected,
}

/// Outbound commands sent from the PTY event loop to the tunnel background task.
#[derive(Debug)]
enum TunnelCommand {
    /// Raw PTY output bytes to forward to viewers.
    Data(Vec<u8>),
    /// Multiplex bus JSON (WebSocket text frame).
    BusText(String),
    /// PTY control JSON (for example terminal resize updates).
    PtyText(String),
    /// Structured agent events JSON (ch: "events").
    EventText(String),
    /// Graceful shutdown.
    Shutdown,
}

/// Handle for sending data into the tunnel. Clone-friendly, non-blocking.
#[derive(Clone)]
pub struct TunnelSender {
    tx: mpsc::Sender<TunnelCommand>,
    session_id: Arc<Mutex<String>>,
}

impl TunnelSender {
    /// Send raw PTY output bytes to the tunnel (non-blocking, drops on full channel).
    pub fn send_data(&self, data: Vec<u8>) {
        let _ = self.tx.try_send(TunnelCommand::Data(data));
    }

    /// Send a routed bus message to other multiplex tunnels for this user (non-blocking).
    pub fn send_bus_routed(
        &self,
        recipient: &str,
        sender: &str,
        body: &str,
        envelope: Option<&crate::message::Envelope>,
    ) {
        let sid = self
            .session_id
            .lock()
            .ok()
            .map(|g| g.clone())
            .unwrap_or_default();
        let json = serde_json::json!({
            "ch": "bus",
            "v": 1,
            "from_session": sid,
            "recipient": recipient,
            "sender": sender,
            "body": body,
            "envelope_json": envelope
                .and_then(|env| serde_json::to_string(env).ok()),
        });
        let _ = self.tx.try_send(TunnelCommand::BusText(json.to_string()));
    }

    pub fn send_terminal_resize(&self, cols: u16, rows: u16) {
        let json = serde_json::json!({
            "ch": "pty",
            "v": 1,
            "event": "resize",
            "cols": cols,
            "rows": rows,
        });
        let _ = self.tx.try_send(TunnelCommand::PtyText(json.to_string()));
    }

    /// Send a structured agent event (non-blocking, drops on full channel).
    pub fn send_event(&self, json: String) {
        let _ = self.tx.try_send(TunnelCommand::EventText(json));
    }

    /// Request graceful shutdown of the tunnel background task.
    pub fn shutdown(&self) {
        let _ = self.tx.try_send(TunnelCommand::Shutdown);
    }
}

/// Receiver for tunnel events. Use in `tokio::select!` via `recv()`.
pub type TunnelReceiver = mpsc::Receiver<TunnelEvent>;

// ---------------------------------------------------------------------------
// Message types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct RegisterMsg<'a> {
    r#type: &'static str,
    session_name: &'a str,
    agent_type: &'a str,
    cwd: &'a str,
    hostname: &'a str,
    nickname: &'a str,
    /// 2 = multiplex (bus on text frames).
    proto: u8,
    cols: u16,
    rows: u16,
}

/// Relay sends JSON text frames during registration handshake only.
#[derive(serde::Deserialize)]
struct RegisterResponse {
    r#type: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Connection parameters (cloneable for reconnect)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ConnectParams {
    token: String,
    session_name: String,
    agent_type: String,
    cwd: String,
    hostname: String,
    nickname: String,
    cols: u16,
    rows: u16,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Connect to the relay and return a sender/receiver pair.
///
/// Spawns a background tokio task that manages the WebSocket lifecycle
/// including heartbeats and automatic reconnection with exponential backoff.
pub async fn connect(
    token: &str,
    session_name: &str,
    agent_type: &str,
    cwd: &str,
    nickname: &str,
    cols: u16,
    rows: u16,
) -> Result<(TunnelSender, TunnelReceiver)> {
    let hostname = gethostname();

    let params = ConnectParams {
        token: token.to_string(),
        session_name: session_name.to_string(),
        agent_type: agent_type.to_string(),
        cwd: cwd.to_string(),
        hostname,
        nickname: nickname.to_string(),
        cols,
        rows,
    };

    // Perform the initial connection synchronously so callers get an immediate error
    // if the relay is unreachable or auth fails.
    let (ws, session_id) = ws_connect_and_register(&params).await?;

    let (cmd_tx, cmd_rx) = mpsc::channel::<TunnelCommand>(CHANNEL_CAPACITY);
    let (evt_tx, evt_rx) = mpsc::channel::<TunnelEvent>(CHANNEL_CAPACITY);
    let session_id_shared = Arc::new(Mutex::new(session_id));

    // Spawn the background I/O loop
    tokio::spawn(tunnel_task(
        ws,
        session_id_shared.clone(),
        params,
        cmd_rx,
        evt_tx,
    ));

    Ok((
        TunnelSender {
            tx: cmd_tx,
            session_id: session_id_shared,
        },
        evt_rx,
    ))
}
