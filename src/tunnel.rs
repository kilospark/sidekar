//! Tunnel client for connecting local PTY sessions to the sidekar relay.
//!
//! Establishes a WSS connection to the relay server, registers the session,
//! and bridges PTY I/O over binary WebSocket frames. JSON text frames carry
//! control messages (register, resize, viewer notifications).
//!
//! The public API returns a `(TunnelSender, TunnelReceiver)` pair designed
//! to integrate into a `tokio::select!` event loop (see `pty.rs`).

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;

const DEFAULT_RELAY_URL: &str = "wss://relay.sidekar.dev/tunnel";
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const RECONNECT_BASE: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);
const CHANNEL_CAPACITY: usize = 256;

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
    /// The tunnel has disconnected (reconnect is happening in the background).
    Disconnected,
}

/// Outbound commands sent from the PTY event loop to the tunnel background task.
#[derive(Debug)]
enum TunnelCommand {
    /// Raw PTY output bytes to forward to viewers.
    Data(Vec<u8>),
    /// Graceful shutdown.
    Shutdown,
}

/// Handle for sending data into the tunnel. Clone-friendly, non-blocking.
#[derive(Clone)]
pub struct TunnelSender {
    tx: mpsc::Sender<TunnelCommand>,
}

impl TunnelSender {
    /// Send raw PTY output bytes to the tunnel (non-blocking, drops on full channel).
    pub fn send_data(&self, data: Vec<u8>) {
        let _ = self.tx.try_send(TunnelCommand::Data(data));
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
) -> Result<(TunnelSender, TunnelReceiver)> {
    let hostname = gethostname();

    let params = ConnectParams {
        token: token.to_string(),
        session_name: session_name.to_string(),
        agent_type: agent_type.to_string(),
        cwd: cwd.to_string(),
        hostname,
        nickname: nickname.to_string(),
    };

    // Perform the initial connection synchronously so callers get an immediate error
    // if the relay is unreachable or auth fails.
    let (ws, session_id) = ws_connect_and_register(&params).await?;

    let (cmd_tx, cmd_rx) = mpsc::channel::<TunnelCommand>(CHANNEL_CAPACITY);
    let (evt_tx, evt_rx) = mpsc::channel::<TunnelEvent>(CHANNEL_CAPACITY);

    // Spawn the background I/O loop
    tokio::spawn(tunnel_task(ws, session_id, params, cmd_rx, evt_tx));

    Ok((TunnelSender { tx: cmd_tx }, evt_rx))
}

// ---------------------------------------------------------------------------
// WebSocket connect + register handshake
// ---------------------------------------------------------------------------

type WsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

async fn ws_connect_and_register(params: &ConnectParams) -> Result<(WsStream, String)> {
    let url = relay_url();

    let mut request = url
        .as_str()
        .into_client_request()
        .with_context(|| format!("invalid relay URL: {url}"))?;

    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", params.token)
            .parse()
            .context("invalid auth header value")?,
    );

    let (mut ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .with_context(|| format!("failed to connect to relay at {url}"))?;

    // Send register message
    let register = RegisterMsg {
        r#type: "register",
        session_name: &params.session_name,
        agent_type: &params.agent_type,
        cwd: &params.cwd,
        hostname: &params.hostname,
        nickname: &params.nickname,
    };
    let register_json = serde_json::to_string(&register).context("serialize register")?;
    ws.send(Message::Text(register_json.into()))
        .await
        .context("send register message")?;

    // Wait for registered response (with timeout)
    let session_id = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    let resp: RegisterResponse =
                        serde_json::from_str(&text).context("parse register response")?;
                    if resp.r#type == "registered" {
                        return resp
                            .session_id
                            .ok_or_else(|| anyhow::anyhow!("registered response missing session_id"));
                    }
                    if resp.r#type == "error" {
                        bail!(
                            "relay rejected registration: {}",
                            resp.error.unwrap_or_else(|| "unknown error".into())
                        );
                    }
                }
                Ok(Message::Close(_)) => bail!("relay closed connection during registration"),
                Err(e) => bail!("websocket error during registration: {e}"),
                _ => {} // ignore ping/pong/binary during handshake
            }
        }
        bail!("relay connection closed before registration completed")
    })
    .await
    .context("registration timed out")?
    .context("registration failed")?;

    Ok((ws, session_id))
}

// ---------------------------------------------------------------------------
// Background I/O task
// ---------------------------------------------------------------------------

async fn tunnel_task(
    ws: WsStream,
    _session_id: String,
    params: ConnectParams,
    mut cmd_rx: mpsc::Receiver<TunnelCommand>,
    evt_tx: mpsc::Sender<TunnelEvent>,
) {
    let mut ws = Some(ws);
    let mut backoff = RECONNECT_BASE;

    loop {
        if let Some(stream) = ws.take() {
            // Run the I/O loop; returns when the connection drops or shutdown is requested.
            let shutdown = io_loop(stream, &mut cmd_rx, &evt_tx).await;
            if shutdown {
                return; // clean shutdown
            }
        }

        // Notify the event loop that we disconnected
        let _ = evt_tx.try_send(TunnelEvent::Disconnected);

        // Reconnect with exponential backoff
        loop {
            tokio::time::sleep(backoff).await;

            match ws_connect_and_register(&params).await {
                Ok((stream, _new_session_id)) => {
                    ws = Some(stream);
                    backoff = RECONNECT_BASE;
                    break;
                }
                Err(_) => {
                    backoff = (backoff * 2).min(RECONNECT_MAX);
                }
            }

            // Check if the PTY event loop has shut down (cmd channel closed)
            if cmd_rx.is_closed() {
                return;
            }
        }
    }
}

/// Run the WebSocket I/O loop. Returns `true` if a clean shutdown was requested.
async fn io_loop(
    ws: WsStream,
    cmd_rx: &mut mpsc::Receiver<TunnelCommand>,
    evt_tx: &mpsc::Sender<TunnelEvent>,
) -> bool {
    let (mut ws_sink, mut ws_stream) = ws.split();
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;

            // PTY output → relay (binary frames only)
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(TunnelCommand::Data(data)) => {
                        if ws_sink.send(Message::Binary(data.into())).await.is_err() {
                            return false;
                        }
                    }
                    Some(TunnelCommand::Shutdown) | None => {
                        let _ = ws_sink.close().await;
                        return true;
                    }
                }
            }

            // Relay → PTY input (binary = viewer keystrokes, text = ignored)
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        let _ = evt_tx.try_send(TunnelEvent::Data(data.into()));
                    }
                    Some(Ok(Message::Ping(data))) => {
                        let _ = ws_sink.send(Message::Pong(data)).await;
                    }
                    Some(Ok(Message::Close(_))) | None => return false,
                    Some(Err(_)) => return false,
                    _ => {}
                }
            }

            // Heartbeat ping
            _ = heartbeat.tick() => {
                if ws_sink.send(Message::Ping(vec![].into())).await.is_err() {
                    return false; // connection lost
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn gethostname() -> String {
    let mut buf = [0u8; 256];
    let ret = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if ret == 0 {
        let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..len]).to_string()
    } else {
        "unknown".to_string()
    }
}
