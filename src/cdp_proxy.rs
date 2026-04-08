//! CDP proxy — daemon-side connection pool and CLI-side proxy client.
//!
//! The daemon holds persistent WebSocket connections to Chrome's CDP,
//! keyed by ws_url. CLI commands connect to the daemon via unix socket
//! and proxy CDP requests through it, avoiding per-call WS overhead.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};

// ---------------------------------------------------------------------------
// Daemon-side: CDP connection pool
// ---------------------------------------------------------------------------

const WS_PING_INTERVAL_SECS: u64 = 15;
const CONNECTION_IDLE_TIMEOUT_SECS: u64 = 120;
const MAX_EVENT_BUFFER: usize = 512;

/// A pending CDP request routed through the daemon.
struct PendingRequest {
    response_tx: oneshot::Sender<Result<Value>>,
}

/// Messages sent from IPC handler to the connection management task.
enum PoolCmd {
    /// Send a CDP method call and get a response.
    Send {
        method: String,
        params: Value,
        session_id: Option<String>,
        response_tx: oneshot::Sender<Result<Value>>,
    },
    /// Subscribe this channel to receive CDP events.
    Subscribe {
        event_tx: mpsc::UnboundedSender<Value>,
    },
}

/// A managed connection to a single Chrome tab.
struct ManagedConn {
    cmd_tx: mpsc::UnboundedSender<PoolCmd>,
    last_used: Arc<AtomicU64>,
    active_clients: Arc<AtomicUsize>,
}

/// Daemon-side CDP connection pool.
pub struct CdpPool {
    connections: HashMap<String, ManagedConn>, // key = ws_url
}

impl CdpPool {
    pub fn new() -> Self {
        Self {
            connections: HashMap::new(),
        }
    }

    /// Get or create a connection for the given ws_url.
    /// Returns a command sender for sending CDP requests.
    fn get_or_create(&mut self, ws_url: &str) -> mpsc::UnboundedSender<PoolCmd> {
        if let Some(conn) = self.connections.get(ws_url) {
            if !conn.cmd_tx.is_closed() {
                conn.last_used.store(epoch_secs(), Ordering::Relaxed);
                return conn.cmd_tx.clone();
            }
        }
        // Remove dead entry before recreating
        self.connections.remove(ws_url);

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let last_used = Arc::new(AtomicU64::new(epoch_secs()));
        let active_clients = Arc::new(AtomicUsize::new(0));
        let ws_url_owned = ws_url.to_string();
        let last_used_clone = last_used.clone();

        tokio::spawn(connection_task(ws_url_owned, cmd_rx, last_used_clone));

        self.connections.insert(
            ws_url.to_string(),
            ManagedConn {
                cmd_tx: cmd_tx.clone(),
                last_used,
                active_clients,
            },
        );

        cmd_tx
    }

    pub fn acquire_client(&mut self, ws_url: &str) {
        let _ = self.get_or_create(ws_url);
        if let Some(conn) = self.connections.get(ws_url) {
            conn.active_clients.fetch_add(1, Ordering::Relaxed);
            conn.last_used.store(epoch_secs(), Ordering::Relaxed);
        }
    }

    pub fn release_client(&mut self, ws_url: &str) {
        if let Some(conn) = self.connections.get(ws_url) {
            conn.active_clients.fetch_sub(1, Ordering::Relaxed);
            conn.last_used.store(epoch_secs(), Ordering::Relaxed);
        }
    }

    /// Remove idle connections that haven't been used recently.
    pub fn reap_idle(&mut self) {
        let now = epoch_secs();
        self.connections.retain(|_url, conn| {
            if conn.cmd_tx.is_closed() {
                return false;
            }
            if conn.active_clients.load(Ordering::Relaxed) > 0 {
                return true;
            }
            let last = conn.last_used.load(Ordering::Relaxed);
            now.saturating_sub(last) < CONNECTION_IDLE_TIMEOUT_SECS
        });
    }

    /// Dispatch a CDP command through the pool. Returns a oneshot receiver
    /// for the response. The caller must NOT hold the pool lock while awaiting.
    pub fn dispatch_cdp(
        &mut self,
        ws_url: &str,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<oneshot::Receiver<Result<Value>>> {
        let cmd_tx = self.get_or_create(ws_url);
        let (response_tx, response_rx) = oneshot::channel();

        cmd_tx
            .send(PoolCmd::Send {
                method: method.to_string(),
                params,
                session_id: session_id.map(String::from),
                response_tx,
            })
            .map_err(|_| anyhow::anyhow!("CDP connection task died"))?;

        Ok(response_rx)
    }

    /// Subscribe to events for a ws_url. Returns a receiver for CDP events.
    pub fn subscribe(&mut self, ws_url: &str) -> mpsc::UnboundedReceiver<Value> {
        let cmd_tx = self.get_or_create(ws_url);
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let _ = cmd_tx.send(PoolCmd::Subscribe { event_tx });

        event_rx
    }
}

// ---------------------------------------------------------------------------
// Connection management task (one per ws_url)
// ---------------------------------------------------------------------------

async fn connection_task(
    ws_url: String,
    mut cmd_rx: mpsc::UnboundedReceiver<PoolCmd>,
    last_used: Arc<AtomicU64>,
) {
    // Try to connect (with timeout to avoid hanging on unresponsive ports)
    let mut ws = match tokio::time::timeout(Duration::from_secs(10), connect_ws(&ws_url)).await {
        Ok(Ok(ws)) => ws,
        Ok(Err(e)) => {
            drain_pending_with_error(&mut cmd_rx, &format!("CDP connect failed: {e}")).await;
            return;
        }
        Err(_) => {
            drain_pending_with_error(&mut cmd_rx, "CDP connect timed out after 10s").await;
            return;
        }
    };

    let mut next_id: u64 = 1;
    let mut pending: HashMap<u64, PendingRequest> = HashMap::new();
    let mut subscribers: Vec<mpsc::UnboundedSender<Value>> = Vec::new();
    let mut ping_interval = tokio::time::interval(Duration::from_secs(WS_PING_INTERVAL_SECS));
    ping_interval.tick().await; // skip immediate tick

    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::protocol::Message;

    loop {
        tokio::select! {
            // Incoming command from IPC handler
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(PoolCmd::Send { method, params, session_id, response_tx }) => {
                        let id = next_id;
                        next_id += 1;
                        last_used.store(epoch_secs(), Ordering::Relaxed);

                        let mut payload = json!({
                            "id": id,
                            "method": method,
                            "params": params,
                        });
                        if let Some(ref sid) = session_id {
                            payload["sessionId"] = json!(sid);
                        }

                        match ws.send(Message::Text(payload.to_string().into())).await {
                            Ok(()) => {
                                pending.insert(id, PendingRequest { response_tx });
                            }
                            Err(e) => {
                                let _ = response_tx.send(Err(anyhow::anyhow!("CDP send failed: {e}")));
                            }
                        }
                    }
                    Some(PoolCmd::Subscribe { event_tx }) => {
                        subscribers.push(event_tx);
                    }
                    None => break, // All senders dropped
                }
            }

            // Incoming WS message from Chrome
            ws_msg = ws.next() => {
                match ws_msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(value) = serde_json::from_str::<Value>(&text) {
                            if let Some(id) = value.get("id").and_then(Value::as_u64) {
                                // Response to a pending request
                                if let Some(req) = pending.remove(&id) {
                                    if let Some(err) = value.get("error") {
                                        let message = err
                                            .get("message")
                                            .and_then(Value::as_str)
                                            .unwrap_or("Unknown CDP error");
                                        let code = err.get("code").and_then(Value::as_i64).unwrap_or_default();
                                        let _ = req.response_tx.send(Err(anyhow::anyhow!("{message} ({code})")));
                                    } else {
                                        let result = value.get("result").cloned().unwrap_or(Value::Null);
                                        let _ = req.response_tx.send(Ok(result));
                                    }
                                }
                            } else if value.get("method").is_some() {
                                // CDP event — broadcast to subscribers
                                subscribers.retain(|tx| tx.send(value.clone()).is_ok());
                            }
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        // keepalive OK
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        // WS closed — fail all pending requests
                        for (_, req) in pending.drain() {
                            let _ = req.response_tx.send(Err(anyhow::anyhow!("WebSocket closed")));
                        }
                        // Notify subscribers
                        let disc = json!({"type": "cdp_disconnected"});
                        subscribers.retain(|tx| tx.send(disc.clone()).is_ok());
                        break;
                    }
                    Some(Ok(_)) => {} // Binary, Ping, Frame — ignore
                    Some(Err(e)) => {
                        for (_, req) in pending.drain() {
                            let _ = req.response_tx.send(Err(anyhow::anyhow!("WebSocket error: {e}")));
                        }
                        break;
                    }
                }
            }

            // Periodic WS ping
            _ = ping_interval.tick() => {
                if ws.send(Message::Ping(Vec::new().into())).await.is_err() {
                    // WS dead
                    for (_, req) in pending.drain() {
                        let _ = req.response_tx.send(Err(anyhow::anyhow!("WebSocket ping failed")));
                    }
                    break;
                }
            }
        }
    }
}

/// Drain all pending pool commands, sending each an error response.
async fn drain_pending_with_error(cmd_rx: &mut mpsc::UnboundedReceiver<PoolCmd>, msg: &str) {
    while let Ok(cmd) = cmd_rx.try_recv() {
        if let PoolCmd::Send { response_tx, .. } = cmd {
            let _ = response_tx.send(Err(anyhow::anyhow!("{}", msg)));
        }
    }
}

async fn connect_ws(
    ws_url: &str,
) -> Result<tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let request = ws_url
        .into_client_request()
        .with_context(|| format!("invalid CDP websocket URL: {ws_url}"))?;

    let host = request.uri().host().unwrap_or("127.0.0.1");
    let port = request.uri().port_u16().unwrap_or(9222);
    let addr = format!("{host}:{port}");

    let tcp = tokio::net::TcpStream::connect(&addr)
        .await
        .with_context(|| format!("failed to connect CDP at {addr}"))?;

    let sock_ref = socket2::SockRef::from(&tcp);
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(30))
        .with_interval(Duration::from_secs(10));
    sock_ref.set_tcp_keepalive(&keepalive)?;

    let (ws, _) = tokio_tungstenite::client_async(request, tcp)
        .await
        .with_context(|| format!("failed to connect CDP websocket: {ws_url}"))?;

    Ok(ws)
}

// ---------------------------------------------------------------------------
// CLI-side: DaemonCdpProxy
// ---------------------------------------------------------------------------

/// CLI-side proxy that sends CDP commands through the daemon.
pub struct DaemonCdpProxy {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    next_req_id: u64,
    pub pending_events: VecDeque<Value>,
    pub auto_dialog: Option<(bool, String)>,
}

impl DaemonCdpProxy {
    /// Connect to the daemon and register for a specific tab.
    pub async fn connect(ws_url: &str) -> Result<Self> {
        let sock_path = crate::daemon::socket_path();
        let stream = tokio::net::UnixStream::connect(&sock_path)
            .await
            .with_context(|| format!("Cannot connect to daemon at {}", sock_path.display()))?;

        let (read_half, mut write_half) = stream.into_split();
        let reader = BufReader::new(read_half);

        // Send registration message
        let reg = json!({
            "type": "cdp_connect",
            "ws_url": ws_url,
        });
        let mut line = serde_json::to_string(&reg)?;
        line.push('\n');
        write_half.write_all(line.as_bytes()).await?;
        write_half.flush().await?;

        Ok(Self {
            reader,
            writer: write_half,
            next_req_id: 1,
            pending_events: VecDeque::new(),
            auto_dialog: None,
        })
    }

    /// Send a CDP command, optionally scoped to a session.
    async fn do_send(
        &mut self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value> {
        let req_id = self.next_req_id;
        self.next_req_id += 1;

        let mut msg = json!({
            "type": "cdp_send",
            "method": method,
            "params": params,
            "req_id": req_id,
        });
        if let Some(sid) = session_id {
            msg["session_id"] = json!(sid);
        }
        // Forward auto_dialog config so daemon can handle dialogs
        if let Some((accept, ref prompt_text)) = self.auto_dialog {
            msg["auto_dialog"] = json!({"accept": accept, "prompt_text": prompt_text});
        }

        let mut line = serde_json::to_string(&msg)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;

        let timeout_duration = super::cdp_send_timeout();
        match tokio::time::timeout(timeout_duration, self.recv_response(req_id)).await {
            Ok(result) => result,
            Err(_) => bail!(
                "CDP method {method} timed out after {}s (via daemon)",
                timeout_duration.as_secs()
            ),
        }
    }

    pub async fn send(&mut self, method: &str, params: Value) -> Result<Value> {
        self.do_send(method, params, None).await
    }

    pub async fn send_to_session(
        &mut self,
        method: &str,
        params: Value,
        session_id: &str,
    ) -> Result<Value> {
        self.do_send(method, params, Some(session_id)).await
    }

    /// Read lines from the daemon socket until we get the response for req_id.
    /// Buffer any events we see along the way.
    async fn recv_response(&mut self, req_id: u64) -> Result<Value> {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self
                .reader
                .read_line(&mut line)
                .await
                .context("daemon socket read error")?;
            if n == 0 {
                bail!("Daemon socket closed");
            }

            let value: Value =
                serde_json::from_str(line.trim()).context("invalid JSON from daemon")?;

            let msg_type = value.get("type").and_then(Value::as_str).unwrap_or("");

            match msg_type {
                "cdp_resp" => {
                    if value.get("req_id").and_then(Value::as_u64) == Some(req_id) {
                        if let Some(err) = value.get("error").and_then(Value::as_str) {
                            bail!("{err}");
                        }
                        return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                    }
                    // Response for a different req_id (shouldn't happen in single-threaded CLI)
                }
                "cdp_event" => {
                    // Buffer the event
                    if self.pending_events.len() >= MAX_EVENT_BUFFER {
                        self.pending_events.pop_front();
                    }
                    self.pending_events.push_back(value);
                }
                "cdp_disconnected" => {
                    bail!("WebSocket closed");
                }
                _ => {}
            }
        }
    }

    pub async fn next_event(&mut self, wait: Duration) -> Result<Option<Value>> {
        if let Some(v) = self.pending_events.pop_front() {
            return Ok(Some(v));
        }

        let mut line = String::new();
        match tokio::time::timeout(wait, self.reader.read_line(&mut line)).await {
            Ok(Ok(0)) => bail!("Daemon socket closed"),
            Ok(Err(e)) => Err(e).context("daemon socket read error"),
            Ok(Ok(_)) => {
                let value: Value = serde_json::from_str(line.trim())?;
                let msg_type = value.get("type").and_then(Value::as_str).unwrap_or("");
                match msg_type {
                    "cdp_event" => Ok(Some(value)),
                    "cdp_disconnected" => bail!("WebSocket closed"),
                    _ => Ok(None),
                }
            }
            Err(_) => Ok(None), // timeout
        }
    }

    pub async fn close(self) {
        // Just drop — the daemon keeps the WS connection alive for reuse
        drop(self);
    }
}

// ---------------------------------------------------------------------------
// Daemon IPC handler for CDP proxy
// ---------------------------------------------------------------------------

/// Handle a CDP proxy connection from a CLI client.
/// Called from daemon's handle_connection when type="cdp_connect".
pub async fn handle_cdp_connection(
    ws_url: String,
    mut reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    mut writer: tokio::net::unix::OwnedWriteHalf,
    pool: Arc<Mutex<CdpPool>>,
) {
    // Subscribe to events for this ws_url
    let mut event_rx = {
        let mut p = pool.lock().await;
        p.acquire_client(&ws_url);
        p.subscribe(&ws_url)
    };

    let mut line = String::new();
    let mut auto_dialog: Option<(bool, String)> = None;

    loop {
        tokio::select! {
            // CLI sends a cdp_send request
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) | Err(_) => break, // CLI disconnected
                    Ok(_) => {
                        if let Ok(cmd) = serde_json::from_str::<Value>(line.trim()) {
                            let cmd_type = cmd.get("type").and_then(Value::as_str).unwrap_or("");
                            if cmd_type == "cdp_send" {
                                let method = cmd.get("method").and_then(Value::as_str).unwrap_or("");
                                let params = cmd.get("params").cloned().unwrap_or(json!({}));
                                let session_id = cmd.get("session_id").and_then(Value::as_str);
                                let req_id = cmd.get("req_id").and_then(Value::as_u64).unwrap_or(0);
                                auto_dialog = cmd
                                    .get("auto_dialog")
                                    .and_then(Value::as_object)
                                    .and_then(|obj| {
                                        Some((
                                            obj.get("accept")?.as_bool()?,
                                            obj.get("prompt_text")
                                                .and_then(Value::as_str)
                                                .unwrap_or_default()
                                                .to_string(),
                                        ))
                                    });

                                // Dispatch under lock (brief), then await response without lock
                                let rx = {
                                    let mut p = pool.lock().await;
                                    p.dispatch_cdp(&ws_url, method, params, session_id)
                                };
                                let mut response_rx = match rx {
                                    Ok(rx) => rx,
                                    Err(e) => {
                                        let response = json!({
                                            "type": "cdp_resp",
                                            "req_id": req_id,
                                            "error": format!("{e:#}"),
                                        });
                                        if write_daemon_line(&mut writer, &response).await.is_err() {
                                            break;
                                        }
                                        line.clear();
                                        continue;
                                    }
                                };

                                loop {
                                    tokio::select! {
                                        result = tokio::time::timeout(Duration::from_secs(120), &mut response_rx) => {
                                            let result = match result {
                                                Ok(Ok(val)) => val,
                                                Ok(Err(_)) => Err(anyhow::anyhow!("CDP response dropped")),
                                                Err(_) => Err(anyhow::anyhow!("CDP response timed out after 120s")),
                                            };

                                            let response = match result {
                                                Ok(val) => json!({
                                                    "type": "cdp_resp",
                                                    "req_id": req_id,
                                                    "result": val,
                                                }),
                                                Err(e) => json!({
                                                    "type": "cdp_resp",
                                                    "req_id": req_id,
                                                    "error": format!("{e:#}"),
                                                }),
                                            };

                                            if write_daemon_line(&mut writer, &response).await.is_err() {
                                                pool.lock().await.release_client(&ws_url);
                                                return;
                                            }
                                            break;
                                        }
                                        event = event_rx.recv() => {
                                            let Some(value) = event else {
                                                pool.lock().await.release_client(&ws_url);
                                                return;
                                            };
                                            match process_pool_event(
                                                &pool,
                                                &ws_url,
                                                &mut writer,
                                                &auto_dialog,
                                                value,
                                            ).await {
                                                Ok(true) => {}
                                                Ok(false) | Err(_) => {
                                                    pool.lock().await.release_client(&ws_url);
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        line.clear();
                    }
                }
            }

            // Forward events from daemon's connection pool to CLI
            event = event_rx.recv() => {
                match event {
                    Some(value) => {
                        match process_pool_event(
                            &pool,
                            &ws_url,
                            &mut writer,
                            &auto_dialog,
                            value,
                        ).await {
                            Ok(true) => {}
                            Ok(false) | Err(_) => {
                                break;
                            }
                        }
                    }
                    None => break, // Connection pool dropped the sender
                }
            }
        }
    }

    pool.lock().await.release_client(&ws_url);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn write_daemon_line(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    value: &Value,
) -> std::io::Result<()> {
    let mut out = serde_json::to_string(value).unwrap_or_default();
    out.push('\n');
    writer.write_all(out.as_bytes()).await?;
    writer.flush().await
}

async fn process_pool_event(
    pool: &Arc<Mutex<CdpPool>>,
    ws_url: &str,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    auto_dialog: &Option<(bool, String)>,
    value: Value,
) -> Result<bool> {
    if value.get("type").and_then(Value::as_str) == Some("cdp_disconnected") {
        write_daemon_line(writer, &json!({"type": "cdp_disconnected"})).await?;
        return Ok(false);
    }

    if value.get("method").and_then(Value::as_str) == Some("Page.javascriptDialogOpening")
        && let Some((accept, prompt_text)) = auto_dialog
    {
        let mut params = json!({ "accept": *accept });
        if !prompt_text.is_empty() {
            params["promptText"] = json!(prompt_text);
        }
        let rx = {
            let mut p = pool.lock().await;
            p.dispatch_cdp(ws_url, "Page.handleJavaScriptDialog", params, None)?
        };
        let _ = tokio::time::timeout(Duration::from_secs(10), rx).await;
        return Ok(true);
    }

    let wrapper = json!({
        "type": "cdp_event",
        "method": value.get("method").cloned().unwrap_or(Value::Null),
        "params": value.get("params").cloned().unwrap_or(Value::Null),
    });
    write_daemon_line(writer, &wrapper).await?;
    Ok(true)
}
