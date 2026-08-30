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
mod viewer;

use transport::*;
pub use viewer::attach_remote_relay_terminal;

const DEFAULT_RELAY_URL: &str = "wss://relay.sidekar.dev/tunnel";
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const RECONNECT_BASE: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);
const CHANNEL_CAPACITY: usize = 256;
/// Ceiling on PTY output queued for a viewer that is not draining.
///
/// Sized well above any normal in-flight burst — a viewer that keeps up never
/// approaches it — but low enough that a stalled socket crosses it long before
/// memory becomes a problem. Past the ceiling sidekar stops streaming bytes and
/// resyncs the viewer from the replay buffer instead of shipping a stream with
/// silent holes in it.
const MAX_QUEUED_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Global output tunnel — lets any module forward println-style output to
// web terminal viewers without threading a TunnelSender through every call.
// ---------------------------------------------------------------------------

static OUTPUT_TUNNEL: Mutex<Option<TunnelSender>> = Mutex::new(None);

/// Borrow the registered output tunnel sender, if any.
pub fn output_tunnel_sender() -> Option<TunnelSender> {
    OUTPUT_TUNNEL.lock().ok().and_then(|g| g.as_ref().cloned())
}

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
    OUTPUT_TUNNEL.lock().ok().is_some_and(|g| g.is_some())
}

/// Print a line to stdout and, if a tunnel is registered, to web viewers.
/// Uses `\r\n` line endings so output is correct in raw terminal mode
/// (cfmakeraw clears OPOST, which disables the kernel's `\n` → `\r\n` translation).
pub fn tunnel_println(text: &str) {
    // Normalize embedded newlines to \r\n, then append a final \r\n
    let normalized = text.replace("\r\n", "\n").replace('\n', "\r\n");
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(normalized.as_bytes());
    let _ = stdout.write_all(b"\r\n");
    let _ = stdout.flush();
    if let Some(ref tx) = *OUTPUT_TUNNEL.lock().unwrap_or_else(|e| e.into_inner()) {
        let mut data = normalized.into_bytes();
        data.extend_from_slice(b"\r\n");
        tx.send_data(data);
    }
}

/// Prompt-style output: stdout write with no trailing newline, mirrored to relay
/// when registered. Prefer this over Rust's `print!` for any REPL user-visible text
/// so web viewers stay in sync with local terminals (`tunnel_println` for full lines).
pub fn tunnel_print(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
    if let Some(ref tx) = *OUTPUT_TUNNEL.lock().unwrap_or_else(|e| e.into_inner()) {
        tx.send_data(text.as_bytes().to_vec());
    }
}

/// Send raw bytes to the tunnel only (no stdout). No-op if no tunnel registered.
pub fn tunnel_send(data: Vec<u8>) {
    if let Some(ref tx) = *OUTPUT_TUNNEL.lock().unwrap_or_else(|e| e.into_inner()) {
        tx.send_data(data);
    }
}

/// Async raw-mode output through a caller-owned stdout handle, mirrored to a
/// specific tunnel sender. Used by the PTY event loop so bus notices serialize
/// with child output instead of racing through global stdout.
pub async fn tunnel_write_async(
    stdout: &mut tokio::io::Stdout,
    tx: Option<&TunnelSender>,
    data: &[u8],
) -> bool {
    use tokio::io::AsyncWriteExt;
    if stdout.write_all(data).await.is_err() {
        return false;
    }
    if stdout.flush().await.is_err() {
        return false;
    }
    if let Some(tx) = tx {
        tx.send_data(data.to_vec());
    }
    true
}

/// Send a structured agent event JSON frame on the `ch:"events"` channel.
/// No-op if no tunnel registered. Used by REPL to symmetrize with
/// PTY-wrapped CLIs whose event parser emits the same frames.
pub fn tunnel_send_event(json: String) {
    if let Some(ref tx) = *OUTPUT_TUNNEL.lock().unwrap_or_else(|e| e.into_inner()) {
        tx.send_event(json);
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
#[allow(clippy::large_enum_variant)]
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
    /// A viewer resized its window and wants the child PTY to match.
    Resize {
        cols: u16,
        rows: u16,
        /// `claim` takes ownership of the size; `update` only applies if the
        /// remote already owns it.
        intent: String,
    },
    /// A viewer attached (or asked to resync) and needs the session replayed.
    ReplayRequested,
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
    /// Session activity for relay-side nudge gating (ch: "activity").
    ActivityText(String),
    /// Graceful shutdown.
    Shutdown,
}

/// Handle for sending data into the tunnel. Clone-friendly, non-blocking.
#[derive(Clone)]
pub struct TunnelSender {
    tx: mpsc::Sender<TunnelCommand>,
    session_id: Arc<Mutex<String>>,
    /// Output bytes accepted but not yet written to the socket.
    queued_bytes: Arc<std::sync::atomic::AtomicUsize>,
    /// Set when output was dropped, so the caller can resync instead of
    /// streaming a corrupted byte stream.
    overflowed: Arc<std::sync::atomic::AtomicBool>,
}

impl TunnelSender {
    /// Send raw PTY output bytes to the tunnel (non-blocking).
    ///
    /// Drops the payload and raises the overflow flag when the viewer is too far
    /// behind. Dropping bytes mid-stream leaves a viewer rendering garbage, so
    /// the flag tells the event loop to replay a clean snapshot once the socket
    /// drains — see [`TunnelSender::take_overflow`].
    pub fn send_data(&self, data: Vec<u8>) {
        use std::sync::atomic::Ordering;

        let queued = self.queued_bytes.load(Ordering::Relaxed);
        if queued.saturating_add(data.len()) > MAX_QUEUED_OUTPUT_BYTES {
            self.overflowed.store(true, Ordering::Relaxed);
            return;
        }

        let len = data.len();
        self.queued_bytes.fetch_add(len, Ordering::Relaxed);
        if self.tx.try_send(TunnelCommand::Data(data)).is_err() {
            self.queued_bytes.fetch_sub(len, Ordering::Relaxed);
            self.overflowed.store(true, Ordering::Relaxed);
        }
    }

    /// Send a replay snapshot, bypassing the backpressure ceiling.
    ///
    /// A resync is what clears the backlog condition; refusing it because the
    /// queue is full would leave the viewer permanently stale.
    pub fn send_resync(&self, data: Vec<u8>) {
        use std::sync::atomic::Ordering;
        let len = data.len();
        self.queued_bytes.fetch_add(len, Ordering::Relaxed);
        if self.tx.try_send(TunnelCommand::Data(data)).is_err() {
            self.queued_bytes.fetch_sub(len, Ordering::Relaxed);
        }
    }

    /// Bytes accepted for delivery but not yet written to the socket.
    pub fn queued_bytes(&self) -> usize {
        self.queued_bytes.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Take the overflow flag: true once per backlog episode.
    pub fn take_overflow(&self) -> bool {
        self.overflowed
            .swap(false, std::sync::atomic::Ordering::Relaxed)
    }

    /// True once the socket has drained enough to be worth resyncing.
    pub fn is_drained(&self) -> bool {
        self.queued_bytes() == 0
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

    /// Publish the terminal modes the agent is currently in.
    ///
    /// The relay prepends this to the scrollback it hands a newly attached
    /// viewer; without it, output captured mid-stream replays into a terminal
    /// that is not in the alternate screen or the agent's key-encoding modes.
    pub fn send_input_mode(&self, preamble: &[u8]) {
        let json = serde_json::json!({
            "ch": "pty",
            "v": 1,
            "event": "mode",
            "preamble": String::from_utf8_lossy(preamble),
        });
        let _ = self.tx.try_send(TunnelCommand::PtyText(json.to_string()));
    }

    /// Tell viewers the session was resynced, so they can clear before replay.
    pub fn send_resync_notice(&self) {
        let json = serde_json::json!({
            "ch": "pty",
            "v": 1,
            "event": "resync",
        });
        let _ = self.tx.try_send(TunnelCommand::PtyText(json.to_string()));
    }

    /// Send a structured agent event (non-blocking, drops on full channel).
    pub fn send_event(&self, json: String) {
        let _ = self.tx.try_send(TunnelCommand::EventText(json));
    }

    /// Publish session activity to the relay (non-blocking).
    pub fn send_activity(&self, state: crate::activity::ActivityState, at: u64) {
        let json = serde_json::json!({
            "ch": "activity",
            "v": 1,
            "state": state.as_str(),
            "at": at,
        });
        let _ = self
            .tx
            .try_send(TunnelCommand::ActivityText(json.to_string()));
    }

    /// Request graceful shutdown of the tunnel background task.
    pub fn shutdown(&self) {
        let _ = self.tx.try_send(TunnelCommand::Shutdown);
    }

    /// Relay-assigned session id for this tunnel host (updated on reconnect).
    /// Used to avoid `/relay attach` round-tripping into the same REPL session.
    pub fn registered_session_id(&self) -> Option<String> {
        let s = self.session_id.lock().ok()?.clone();
        (!s.is_empty()).then_some(s)
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
    let queued_bytes = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    // Spawn the background I/O loop
    tokio::spawn(tunnel_task(
        ws,
        session_id_shared.clone(),
        params,
        cmd_rx,
        evt_tx,
        queued_bytes.clone(),
    ));

    Ok((
        TunnelSender {
            tx: cmd_tx,
            session_id: session_id_shared,
            queued_bytes,
            overflowed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
        evt_rx,
    ))
}
