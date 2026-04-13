use crate::*;

mod tabs;

pub use tabs::*;

/// Direct WebSocket connection to Chrome's CDP (used when daemon unavailable).
pub struct DirectCdp {
    pub ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    pub next_id: u64,
    pub pending_events: VecDeque<Value>,
    /// If set, auto-handle JS dialogs via CDP Page.handleJavaScriptDialog.
    pub auto_dialog: Option<(bool, String)>, // (accept, promptText)
    closed: bool,
}

impl DirectCdp {
    pub async fn connect(ws_url: &str) -> Result<Self> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let request = ws_url
            .into_client_request()
            .with_context(|| format!("invalid CDP websocket URL: {ws_url}"))?;

        let host = request.uri().host().unwrap_or("127.0.0.1");
        let port = request.uri().port_u16().unwrap_or(9222);
        let addr = format!("{host}:{port}");

        let tcp_stream = tokio::net::TcpStream::connect(&addr)
            .await
            .with_context(|| format!("failed to connect CDP at {addr}"))?;

        let sock_ref = socket2::SockRef::from(&tcp_stream);
        let keepalive = socket2::TcpKeepalive::new()
            .with_time(Duration::from_secs(30))
            .with_interval(Duration::from_secs(10));
        sock_ref.set_tcp_keepalive(&keepalive)?;

        let (ws, _) = tokio_tungstenite::client_async(request, tcp_stream)
            .await
            .with_context(|| format!("failed to connect CDP websocket: {ws_url}"))?;

        Ok(Self {
            ws,
            next_id: 1,
            pending_events: VecDeque::new(),
            auto_dialog: None,
            closed: false,
        })
    }

    async fn do_send(
        &mut self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let mut payload = json!({
            "id": id,
            "method": method,
            "params": params,
        });
        if let Some(sid) = session_id {
            payload["sessionId"] = json!(sid);
        }

        let context = if let Some(sid) = session_id {
            format!("failed to send CDP method {method} to session {sid}")
        } else {
            format!("failed to send CDP method {method}")
        };

        self.ws
            .send(Message::Text(payload.to_string().into()))
            .await
            .with_context(|| context)?;

        let timeout_duration = cdp_send_timeout();
        match timeout(timeout_duration, self.recv_response(id)).await {
            Ok(result) => result,
            Err(_) => bail!(
                "CDP method {method} timed out after {}s",
                timeout_duration.as_secs()
            ),
        }
    }

    pub async fn send_to_session(
        &mut self,
        method: &str,
        params: Value,
        session_id: &str,
    ) -> Result<Value> {
        self.do_send(method, params, Some(session_id)).await
    }

    pub async fn send(&mut self, method: &str, params: Value) -> Result<Value> {
        self.do_send(method, params, None).await
    }

    async fn recv_response(&mut self, id: u64) -> Result<Value> {
        while let Some(msg) = self.ws.next().await {
            let value = self
                .parse_ws_message(msg.context("CDP websocket read error")?)?
                .ok_or_else(|| anyhow!("WebSocket closed"))?;

            if value.get("method").and_then(Value::as_str) == Some("Page.javascriptDialogOpening")
                && let Some((accept, prompt_text)) = &self.auto_dialog
            {
                let dialog_id = self.next_id;
                self.next_id += 1;
                let mut params = json!({ "accept": *accept });
                if !prompt_text.is_empty() {
                    params["promptText"] = Value::String(prompt_text.clone());
                }
                let payload = json!({
                    "id": dialog_id,
                    "method": "Page.handleJavaScriptDialog",
                    "params": params,
                });
                if let Err(e) = self
                    .ws
                    .send(Message::Text(payload.to_string().into()))
                    .await
                {
                    wlog!("failed auto-handling dialog: {e}");
                }
                let dialog_type = value
                    .pointer("/params/type")
                    .and_then(Value::as_str)
                    .unwrap_or("dialog");
                let msg_text = value
                    .pointer("/params/message")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                wlog!(
                    "Auto-{}ed {}: \"{}\"",
                    if *accept { "accept" } else { "dismiss" },
                    dialog_type,
                    msg_text
                );
                continue;
            }

            if value.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = value.get("error") {
                    let message = err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Unknown CDP error");
                    let code = err.get("code").and_then(Value::as_i64).unwrap_or_default();
                    bail!("{message} ({code})");
                }
                return Ok(value.get("result").cloned().unwrap_or(Value::Null));
            }
            if value.get("method").is_some() {
                if self.pending_events.len() >= MAX_PENDING_EVENTS {
                    if self.pending_events.len() == MAX_PENDING_EVENTS {
                        wlog!(
                            "CDP event queue full ({MAX_PENDING_EVENTS}), dropping oldest events"
                        );
                    }
                    self.pending_events.pop_front();
                }
                self.pending_events.push_back(value);
            }
        }

        bail!("WebSocket closed")
    }

    pub async fn next_event(&mut self, wait: Duration) -> Result<Option<Value>> {
        if let Some(v) = self.pending_events.pop_front() {
            return Ok(Some(v));
        }

        let msg = match timeout(wait, self.ws.next()).await {
            Ok(maybe) => maybe,
            Err(_) => return Ok(None),
        };

        match msg {
            Some(raw) => self.parse_ws_message(raw.context("CDP websocket read error")?),
            None => Ok(None),
        }
    }

    pub fn parse_ws_message(&self, msg: Message) -> Result<Option<Value>> {
        match msg {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(&text)
                    .with_context(|| format!("invalid CDP JSON message: {text}"))?;
                Ok(Some(value))
            }
            Message::Binary(bin) => {
                let text = String::from_utf8_lossy(&bin);
                let value: Value = serde_json::from_str(&text)
                    .with_context(|| format!("invalid CDP JSON message: {text}"))?;
                Ok(Some(value))
            }
            Message::Close(_) => Ok(None),
            _ => Ok(Some(Value::Null)),
        }
    }

    pub async fn close(mut self) {
        self.closed = true;
        if let Err(e) = self.ws.close(None).await {
            wlog!("CDP close failed: {e}");
        }
    }
}

impl Drop for DirectCdp {
    fn drop(&mut self) {
        if !self.closed {
            crate::broker::try_log_event(
                "debug",
                "cdp",
                "DirectCdp dropped without close()",
                None,
            );
        }
    }
}

/// Unified CDP handle. Commands use this type; the connection may be
/// a direct WebSocket or proxied through the sidekar daemon.
#[allow(clippy::large_enum_variant)]
pub enum CdpClient {
    Direct(DirectCdp),
    Proxied(cdp_proxy::DaemonCdpProxy),
}

impl CdpClient {
    pub async fn send(&mut self, method: &str, params: Value) -> Result<Value> {
        match self {
            Self::Direct(d) => d.send(method, params).await,
            Self::Proxied(p) => p.send(method, params).await,
        }
    }

    pub async fn send_to_session(
        &mut self,
        method: &str,
        params: Value,
        session_id: &str,
    ) -> Result<Value> {
        match self {
            Self::Direct(d) => d.send_to_session(method, params, session_id).await,
            Self::Proxied(p) => p.send_to_session(method, params, session_id).await,
        }
    }

    pub async fn next_event(&mut self, wait: Duration) -> Result<Option<Value>> {
        match self {
            Self::Direct(d) => d.next_event(wait).await,
            Self::Proxied(p) => p.next_event(wait).await,
        }
    }

    pub async fn close(self) {
        match self {
            Self::Direct(d) => d.close().await,
            Self::Proxied(p) => p.close().await,
        }
    }

    pub fn set_auto_dialog(&mut self, accept: bool, prompt_text: String) {
        match self {
            Self::Direct(d) => d.auto_dialog = Some((accept, prompt_text)),
            Self::Proxied(p) => p.auto_dialog = Some((accept, prompt_text)),
        }
    }

    pub fn clear_auto_dialog(&mut self) {
        match self {
            Self::Direct(d) => d.auto_dialog = None,
            Self::Proxied(p) => p.auto_dialog = None,
        }
    }

    pub fn pending_events_mut(&mut self) -> &mut VecDeque<Value> {
        match self {
            Self::Direct(d) => &mut d.pending_events,
            Self::Proxied(p) => &mut p.pending_events,
        }
    }
}
