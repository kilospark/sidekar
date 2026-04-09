use super::*;

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

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
