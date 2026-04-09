use crate::*;

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

            if value.get("method").and_then(Value::as_str) == Some("Page.javascriptDialogOpening") {
                if let Some((accept, prompt_text)) = &self.auto_dialog {
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
        if !self.closed && std::env::var_os("SIDEKAR_DEBUG").is_some() {
            eprintln!("sidekar: DirectCdp dropped without close()");
        }
    }
}

/// Unified CDP handle. Commands use this type; the connection may be
/// a direct WebSocket or proxied through the sidekar daemon.
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

pub async fn open_cdp(ctx: &mut AppContext) -> Result<CdpClient> {
    match open_cdp_once(ctx).await {
        Ok(cdp) => Ok(cdp),
        Err(first_err) => {
            let msg = first_err.to_string();
            if msg.contains("WebSocket closed")
                || msg.contains("Connection refused")
                || msg.contains("failed to connect")
                || msg.contains("daemon")
            {
                wlog!("CDP connection failed ({msg}), retrying...");
                sleep(Duration::from_millis(500)).await;
                open_cdp_once(ctx)
                    .await
                    .with_context(|| format!("CDP retry also failed (original: {msg})"))
            } else {
                Err(first_err)
            }
        }
    }
}

async fn open_cdp_once(ctx: &mut AppContext) -> Result<CdpClient> {
    let tab = connect_to_tab(ctx).await?;
    if let Some(lock) = check_tab_lock(ctx, &tab.id)? {
        let sid = ctx.require_session_id()?;
        if lock.session_id != sid {
            let remaining = ((lock.expires - now_epoch_ms()).max(0) / 1000) as i64;
            bail!(
                "Tab is locked by session {} (expires in {}s). Use a different tab or wait.",
                lock.session_id,
                remaining
            );
        }
    }
    let ws_url = tab
        .web_socket_debugger_url
        .ok_or_else(|| anyhow!("No active tab for this session. Navigate to a URL first."))?;

    if daemon::is_running() {
        match cdp_proxy::DaemonCdpProxy::connect(&ws_url).await {
            Ok(proxy) => return Ok(CdpClient::Proxied(proxy)),
            Err(_) => {}
        }
    }

    Ok(CdpClient::Direct(DirectCdp::connect(&ws_url).await?))
}

pub async fn connect_to_tab(ctx: &mut AppContext) -> Result<DebugTab> {
    fn format_tab_candidates(tabs: &[DebugTab], owned_ids: &[String]) -> String {
        let owned = tabs
            .iter()
            .filter(|t| owned_ids.iter().any(|id| id == &t.id))
            .take(5)
            .map(|t| {
                let label = t
                    .title
                    .as_deref()
                    .or(t.url.as_deref())
                    .unwrap_or("(untitled)");
                format!("{} ({label})", t.id)
            })
            .collect::<Vec<_>>();
        if owned.is_empty() {
            "none".to_string()
        } else {
            owned.join(", ")
        }
    }

    if let Some(ref target_id) = ctx.override_tab_id {
        let tabs = get_debug_tabs(ctx).await?;
        let tab = tabs
            .iter()
            .find(|t| t.id == *target_id)
            .cloned()
            .ok_or_else(|| anyhow!("Tab not found: {target_id}"))?;
        if tab.web_socket_debugger_url.is_none() {
            bail!("Tab {target_id} has no webSocketDebuggerUrl");
        }
        return Ok(tab);
    }

    let mut state = ctx.load_session_state()?;
    let tabs = get_debug_tabs(ctx).await?;

    let live_ids: HashSet<&str> = tabs.iter().map(|t| t.id.as_str()).collect();
    let before = state.tabs.len();
    state.tabs.retain(|id| live_ids.contains(id.as_str()));
    if state.tabs.len() < before {
        wlog!(
            "Pruned {} stale tab ID(s) from session state",
            before - state.tabs.len()
        );
    }
    ctx.save_session_state(&state)?;

    let selected = if let Some(active_id) = state.active_tab_id.clone() {
        let tab = tabs
            .iter()
            .find(|t| t.id == active_id && t.web_socket_debugger_url.is_some())
            .cloned();

        if let Some(tab) = tab {
            tab
        } else {
            state.active_tab_id = None;
            ctx.save_session_state(&state)?;
            if state.tabs.is_empty() {
                bail!(
                    "Active tab {active_id} is gone and this session has no remaining tabs. Run `sidekar new-tab` or pass `--tab <id>`."
                );
            }
            let remaining = format_tab_candidates(&tabs, &state.tabs);
            bail!(
                "Active tab {active_id} is gone. Remaining session tabs: {remaining}. Run `sidekar tab <id>` or pass `--tab <id>`."
            );
        }
    } else if state.tabs.is_empty() {
        bail!("No active tab for this session. Run `sidekar new-tab` or pass `--tab <id>`.");
    } else {
        let remaining = format_tab_candidates(&tabs, &state.tabs);
        bail!(
            "No active tab is selected for this session. Remaining session tabs: {remaining}. Run `sidekar tab <id>` or pass `--tab <id>`."
        );
    };
    state.active_tab_id = Some(selected.id.clone());
    ctx.save_session_state(&state)?;

    Ok(selected)
}

pub async fn verify_cdp_ready(ctx: &AppContext) -> Result<()> {
    let tabs = get_debug_tabs(ctx).await?;
    let tab = tabs.first().ok_or_else(|| anyhow!("No tabs available"))?;
    let ws_url = tab
        .web_socket_debugger_url
        .as_ref()
        .ok_or_else(|| anyhow!("No WebSocket URL"))?;
    let mut cdp = DirectCdp::connect(ws_url).await?;
    let result = cdp.send("Browser.getVersion", json!({})).await;
    cdp.close().await;
    result.map(|_| ())
}

pub async fn get_debug_tabs(ctx: &AppContext) -> Result<Vec<DebugTab>> {
    let body = http_get_text(ctx, "/json").await?;
    serde_json::from_str::<Vec<DebugTab>>(&body).context("Failed to parse Chrome debug info")
}

pub async fn create_new_tab(ctx: &AppContext, url: Option<&str>) -> Result<DebugTab> {
    let suffix = match url {
        Some(raw) if !raw.is_empty() => {
            let encoded = urlencoding::encode(raw);
            format!("/json/new?{encoded}")
        }
        _ => "/json/new".to_string(),
    };
    let body = http_put_text(ctx, &suffix).await?;
    serde_json::from_str::<DebugTab>(&body).context("Failed to create new tab")
}

pub async fn create_new_window(ctx: &AppContext, url: Option<&str>) -> Result<DebugTab> {
    let tabs = get_debug_tabs(ctx).await?;
    let any_tab = tabs
        .first()
        .ok_or_else(|| anyhow!("No existing tab to connect through"))?;
    let ws_url = any_tab
        .web_socket_debugger_url
        .as_ref()
        .ok_or_else(|| anyhow!("No WebSocket URL for existing tab"))?;
    let mut cdp = DirectCdp::connect(ws_url).await?;
    let result = cdp
        .send(
            "Target.createTarget",
            json!({
                "url": url.unwrap_or("about:blank"),
                "newWindow": true
            }),
        )
        .await?;

    let target_id = result
        .get("targetId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("No targetId in createTarget response"))?;

    cdp.close().await;

    for _ in 0..5 {
        let all_tabs = get_debug_tabs(ctx).await?;
        if let Some(tab) = all_tabs.into_iter().find(|t| t.id == target_id) {
            return Ok(tab);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    bail!("New window tab not found in tab list after retries")
}

pub async fn detect_browser_from_port(ctx: &AppContext) -> Option<String> {
    let body = http_get_text(ctx, "/json/version").await.ok()?;
    let info: Value = serde_json::from_str(&body).ok()?;
    let browser = info.get("Browser").and_then(Value::as_str).unwrap_or("");
    let user_agent = info.get("User-Agent").and_then(Value::as_str).unwrap_or("");

    let name = if user_agent.contains("Edg/") {
        "Microsoft Edge"
    } else if user_agent.contains("Brave/") || user_agent.contains("brave") {
        "Brave Browser"
    } else if user_agent.contains("OPR/") || user_agent.contains("Opera") {
        "Opera"
    } else if user_agent.contains("Vivaldi/") {
        "Vivaldi"
    } else if user_agent.contains("Arc/") || user_agent.contains("arc ") {
        "Arc"
    } else if browser.starts_with("Chrome/") || browser.starts_with("HeadlessChrome/") {
        "Google Chrome"
    } else if browser.starts_with("Chromium/") {
        "Chromium"
    } else {
        return None;
    };
    Some(name.to_string())
}

pub async fn get_window_id_for_target(_ctx: &AppContext, tab_ws_url: &str) -> Result<i64> {
    let mut cdp = DirectCdp::connect(tab_ws_url).await?;
    let result = cdp.send("Browser.getWindowForTarget", json!({})).await?;
    cdp.close().await;
    result
        .get("windowId")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("No windowId in Browser.getWindowForTarget response"))
}

pub async fn minimize_window_by_id(
    _ctx: &AppContext,
    tab_ws_url: &str,
    window_id: i64,
) -> Result<()> {
    let mut cdp = DirectCdp::connect(tab_ws_url).await?;
    cdp.send(
        "Browser.setWindowBounds",
        json!({"windowId": window_id, "bounds": {"windowState": "minimized"}}),
    )
    .await?;
    cdp.close().await;
    Ok(())
}

pub async fn restore_window_by_id(
    _ctx: &AppContext,
    tab_ws_url: &str,
    window_id: i64,
) -> Result<()> {
    let mut cdp = DirectCdp::connect(tab_ws_url).await?;
    cdp.send(
        "Browser.setWindowBounds",
        json!({"windowId": window_id, "bounds": {"windowState": "normal"}}),
    )
    .await?;
    cdp.close().await;
    Ok(())
}

pub async fn http_get_text(ctx: &AppContext, path: &str) -> Result<String> {
    let url = format!("http://{}:{}{}", ctx.cdp_host, ctx.cdp_port, path);
    let resp = ctx
        .http
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url} failed"))?;
    timeout(Duration::from_secs(10), resp.text())
        .await
        .with_context(|| format!("GET {url} body read timed out"))?
        .with_context(|| format!("GET {url} body read failed"))
}

pub async fn http_put_text(ctx: &AppContext, path: &str) -> Result<String> {
    let url = format!("http://{}:{}{}", ctx.cdp_host, ctx.cdp_port, path);
    let resp = ctx
        .http
        .put(&url)
        .send()
        .await
        .with_context(|| format!("PUT {url} failed"))?;
    timeout(Duration::from_secs(10), resp.text())
        .await
        .with_context(|| format!("PUT {url} body read timed out"))?
        .with_context(|| format!("PUT {url} body read failed"))
}
