use super::{admin, *};

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::protocol::Message;

/// Largest request body the admin API will read. Prompts are the only
/// thing posted here and the longest one ships at ~5 KB, so this is
/// generous while still bounding what an unauthenticated localhost
/// caller can make the daemon allocate.
const MAX_BODY_BYTES: usize = 256 * 1024;

pub(super) struct RequestHead {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: std::collections::HashMap<String, String>,
    /// Byte length of the request line plus headers, including the
    /// blank line that terminates them. The body starts here.
    pub head_len: usize,
    pub content_length: usize,
}

impl RequestHead {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(|s| s.as_str())
    }
}

/// Parse the request line and headers out of a peeked buffer.
///
/// Returns `None` when the headers are incomplete, which for the peek
/// buffer means either a truncated request or a header block larger
/// than the buffer. Header names are lowercased; values are trimmed.
pub(super) fn parse_request_head(raw: &str) -> Option<RequestHead> {
    let (head, terminator_len) = match raw.find("\r\n\r\n") {
        Some(i) => (&raw[..i], 4),
        None => (&raw[..raw.find("\n\n")?], 2),
    };

    let mut lines = head.split('\n').map(|l| l.trim_end_matches('\r'));
    let first = lines.next()?;
    let mut parts = first.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?;
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    };

    let mut headers = std::collections::HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    let content_length = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);

    Some(RequestHead {
        method,
        path,
        query,
        headers,
        head_len: head.len() + terminator_len,
        content_length,
    })
}

/// Consume the request off the socket and return the body.
///
/// Called for every admin route, including GETs with no body: closing a
/// socket that still holds unread inbound bytes makes the peer send RST,
/// which throws away the response we just wrote. Non-admin paths keep
/// the peek-only behaviour so the `/ext` WebSocket upgrade can still
/// hand tungstenite an untouched stream.
async fn read_request_body(
    stream: &mut tokio::net::TcpStream,
    head: &RequestHead,
) -> Result<String, (u16, &'static str)> {
    let mut head_buf = vec![0u8; head.head_len];
    stream
        .read_exact(&mut head_buf)
        .await
        .map_err(|_| (400, "Bad Request"))?;

    if head.header("transfer-encoding").is_some() {
        return Err((411, "Length Required"));
    }
    if head.content_length > MAX_BODY_BYTES {
        return Err((413, "Payload Too Large"));
    }
    if head.content_length == 0 {
        return Ok(String::new());
    }

    let mut body = vec![0u8; head.content_length];
    stream
        .read_exact(&mut body)
        .await
        .map_err(|_| (400, "Bad Request"))?;
    String::from_utf8(body).map_err(|_| (400, "Bad Request"))
}

/// Request target path for `/health` probes (Chrome extension port discovery).
/// Query string is stripped so `/health?x=1` still matches.
fn health_request_path(first_line: &str) -> Option<&str> {
    let mut parts = first_line.split_whitespace();
    parts.next()?; // method
    let target = parts.next()?;
    Some(target.split('?').next().unwrap_or(target))
}

fn is_health_probe(first_line: &str) -> bool {
    matches!(health_request_path(first_line), Some("/health"))
}

/// Port range for the localhost HTTP/WebSocket listener used by extensions.
const HTTP_PORT_START: u16 = 21517;
const HTTP_PORT_END: u16 = 21527;

pub(super) fn bind_http_listener() -> Option<(std::net::TcpListener, u16)> {
    for port in HTTP_PORT_START..=HTTP_PORT_END {
        let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
        match std::net::TcpListener::bind(addr) {
            Ok(listener) => {
                listener.set_nonblocking(true).ok();
                return Some((listener, port));
            }
            Err(_) => continue,
        }
    }
    crate::broker::try_log_error(
        "daemon",
        &format!("could not bind HTTP listener on ports {HTTP_PORT_START}-{HTTP_PORT_END}"),
        None,
    );
    None
}

pub(super) async fn accept_http_connections(
    listener: tokio::net::TcpListener,
    state: Arc<Mutex<DaemonState>>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let s = state.clone();
                tokio::spawn(handle_http_connection(stream, s));
            }
            Err(e) => {
                crate::broker::try_log_error("http", "accept error", Some(&format!("{e:#}")));
            }
        }
    }
}

/// Parse a request off the socket and let the admin router answer it.
///
/// Returns false when the path is not an admin route, leaving the stream
/// untouched so the `/ext` WebSocket upgrade can still take it over.
pub(super) async fn serve_admin_request(
    stream: &mut tokio::net::TcpStream,
    http_port: u16,
    ext_status: Value,
) -> bool {
    let mut buf = [0u8; 4096];
    let n = match stream.peek(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return false,
    };
    let Ok(raw) = std::str::from_utf8(&buf[..n]) else {
        return false;
    };
    let Some(head) = parse_request_head(raw) else {
        let response = "HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n";
        let _ = stream.write_all(response.as_bytes()).await;
        return true;
    };

    if !admin::is_ui_route(&head.path) {
        return false;
    }

    let body = match read_request_body(stream, &head).await {
        Ok(b) => b,
        Err((status, reason)) => {
            let response = format!("HTTP/1.1 {status} {reason}\r\nConnection: close\r\n\r\n");
            let _ = stream.write_all(response.as_bytes()).await;
            return true;
        }
    };

    let request = admin::AdminRequest {
        method: &head.method,
        path: &head.path,
        query: &head.query,
        origin: head.header("origin"),
        host: head.header("host"),
        content_type: head.header("content-type"),
        body: &body,
        http_port,
        ext_status,
    };
    admin::handle_admin_request(request, stream).await
}

async fn handle_http_connection(mut stream: tokio::net::TcpStream, state: Arc<Mutex<DaemonState>>) {
    let mut buf = [0u8; 4096];
    let n = match stream.peek(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let request = match std::str::from_utf8(&buf[..n]) {
        Ok(s) => s,
        Err(_) => return,
    };

    let first_line = request.lines().next().unwrap_or("");

    // Chrome MV3 extension fetch() from the service worker is cross-origin vs
    // http://127.0.0.1 — newer Chrome sends a Private Network Access OPTIONS
    // preflight before GET. Without `Access-Control-Allow-Private-Network` the
    // probe fails and the extension never discovers the daemon port.
    if is_health_probe(first_line) {
        let method = first_line.split_whitespace().next().unwrap_or("");
        if method.eq_ignore_ascii_case("OPTIONS") {
            let response = concat!(
                "HTTP/1.1 204 No Content\r\n",
                "Access-Control-Allow-Origin: *\r\n",
                "Access-Control-Allow-Methods: GET, OPTIONS\r\n",
                "Access-Control-Allow-Headers: *\r\n",
                "Access-Control-Max-Age: 86400\r\n",
                "Access-Control-Allow-Private-Network: true\r\n",
                "Connection: close\r\n",
                "\r\n",
            );
            let _ = stream.write_all(response.as_bytes()).await;
            return;
        }
        if method.eq_ignore_ascii_case("GET") {
            let body = r#"{"sidekar":true}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: application/json\r\n\
                 x-sidekar: 1\r\n\
                 Access-Control-Allow-Origin: *\r\n\
                 Access-Control-Allow-Private-Network: true\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\
                 \r\n\
                 {}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
            return;
        }
    }

    let http_port = state.lock().await.http_port;
    let ext_status = {
        let s = state.lock().await;
        crate::ext::get_status(&s.ext_state).await
    };
    if serve_admin_request(&mut stream, http_port, ext_status).await {
        return;
    }

    if first_line.contains("/ext") {
        let ext_state = state.lock().await.ext_state.clone();
        match tokio_tungstenite::accept_async(stream).await {
            Ok(ws) => handle_ext_websocket(ws, ext_state).await,
            Err(e) => {
                crate::broker::try_log_error(
                    "http",
                    "WS handshake failed",
                    Some(&format!("{e:#}")),
                );
            }
        }
        return;
    }

    let response = "HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n";
    let _ = stream.write_all(response.as_bytes()).await;
}

async fn handle_ext_websocket(
    ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    ext_state: SharedExtState,
) {
    let (mut ws_tx, mut ws_rx) = ws.split();

    let welcome = json!({"type": "welcome", "version": env!("CARGO_PKG_VERSION")});
    if ws_tx
        .send(Message::Text(welcome.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    let (ext_token, agent_id, browser_name, install_id, ext_version) = loop {
        match ws_rx.next().await {
            Some(Ok(Message::Text(text))) => {
                if let Ok(val) = serde_json::from_str::<Value>(&text)
                    && val.get("type").and_then(|v| v.as_str()) == Some("bridge_register")
                {
                    let token = val
                        .get("token")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let aid = val
                        .get("agent_id")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    let browser = val
                        .get("browser")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Chrome")
                        .to_string();
                    let install_id = val
                        .get("installId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    // Extension reports its manifest.json version in
                    // bridge_register. We store it on the connection
                    // for diagnostics, and log a warning here if it
                    // differs from the daemon's own CARGO_PKG_VERSION.
                    // Chrome auto-updates the extension independently
                    // of the binary, so these can drift; drift is not
                    // fatal (the wire protocol is backward-compatible
                    // within a minor series) but worth surfacing
                    // because a severe mismatch could change the
                    // shape of a new event type that only one side
                    // knows about.
                    let ev = val
                        .get("version")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    if let Some(ref v) = ev
                        && v != env!("CARGO_PKG_VERSION")
                    {
                        crate::broker::try_log_event(
                            "warn",
                            "ext",
                            &format!(
                                "extension version {v} differs from daemon {}; \
                                 restart Chrome after `sidekar update` to re-sync",
                                env!("CARGO_PKG_VERSION")
                            ),
                            None,
                        );
                    }
                    break (token, aid, browser, install_id, ev);
                }
            }
            _ => return,
        }
    };

    let cli_logged_in = crate::auth::auth_token().is_some();
    let user_id = if ext_token.is_empty() {
        let fail = json!({
            "type": "auth_fail",
            "reason": "No extension token — sign in from the extension popup.",
            "cli_logged_in": cli_logged_in,
        });
        let _ = ws_tx.send(Message::Text(fail.to_string().into())).await;
        return;
    } else {
        use crate::ext::VerifyResult;
        match tokio::task::spawn_blocking({
            let token = ext_token.clone();
            move || crate::ext::verify_ext_token(&token)
        })
        .await
        {
            Ok(VerifyResult::Ok(uid)) => uid,
            Ok(VerifyResult::InvalidToken(reason)) => {
                let fail = json!({
                    "type": "auth_fail",
                    "reason": reason,
                    "clear_token": true,
                    "cli_logged_in": cli_logged_in,
                });
                let _ = ws_tx.send(Message::Text(fail.to_string().into())).await;
                return;
            }
            Ok(VerifyResult::TransientError(reason)) => {
                let fail = json!({
                    "type": "auth_fail",
                    "reason": reason,
                    "cli_logged_in": cli_logged_in,
                });
                let _ = ws_tx.send(Message::Text(fail.to_string().into())).await;
                return;
            }
            Err(_) => {
                let fail = json!({
                    "type": "auth_fail",
                    "reason": "Internal error during verification — retrying.",
                    "cli_logged_in": cli_logged_in,
                });
                let _ = ws_tx.send(Message::Text(fail.to_string().into())).await;
                return;
            }
        }
    };

    let (conn_id, mut bridge_rx, profile) = crate::ext::register_bridge_ws(
        &ext_state,
        user_id.clone(),
        agent_id,
        browser_name.clone(),
        install_id.clone(),
        ext_version,
    )
    .await;

    let ok = json!({"type": "auth_ok", "cli_logged_in": cli_logged_in, "profile": profile});
    if ws_tx
        .send(Message::Text(ok.to_string().into()))
        .await
        .is_err()
    {
        crate::ext::disconnect_bridge_by_id(&ext_state, conn_id).await;
        return;
    }

    crate::broker::try_log_event(
        "info",
        "ext",
        &format!("bridge connected (conn: {conn_id}, browser: {browser_name}, user: {user_id})"),
        None,
    );

    let ka_state = ext_state.clone();
    let ka_conn_id = conn_id;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
        interval.tick().await;
        loop {
            interval.tick().await;
            let now = crate::message::epoch_secs();
            let should_disconnect;
            {
                let s = ka_state.lock().await;
                match s.connections.get(&ka_conn_id) {
                    Some(conn) => {
                        let busy = !conn.pending.is_empty() || conn.cli_exec_inflight > 0;
                        should_disconnect = !busy && now - conn.last_contact > 30;
                        if !should_disconnect {
                            let ping =
                                serde_json::to_string(&json!({"type":"ping"})).unwrap_or_default();
                            let _ = conn.bridge_tx.send(ping);
                        }
                    }
                    None => break,
                }
            }
            if should_disconnect {
                crate::broker::try_log_event(
                    "warn",
                    "ext",
                    &format!("WS keepalive timeout (conn {ka_conn_id})"),
                    None,
                );
                crate::ext::disconnect_bridge_by_id(&ka_state, ka_conn_id).await;
                break;
            }
        }
    });

    loop {
        tokio::select! {
            outbound = bridge_rx.recv() => {
                let Some(outbound) = outbound else { break };
                if ws_tx.send(Message::Text(outbound.into())).await.is_err() {
                    break;
                }
            }
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(val) = serde_json::from_str::<Value>(&text) {
                            crate::ext::touch_connection(&ext_state, conn_id).await;
                            let msg_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            if msg_type == "pong" {
                                continue;
                            }
                            if msg_type == "net_passive_event" {
                                let tab_id = val
                                    .get("tabId")
                                    .and_then(|v| v.as_i64());
                                let tab_url = val
                                    .get("tabUrl")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let frame_url = val
                                    .get("frameUrl")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let dropped = val
                                    .get("dropped")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                let events: Vec<Value> = val
                                    .get("events")
                                    .and_then(|v| v.as_array())
                                    .cloned()
                                    .unwrap_or_default();
                                crate::ext::ingest_passive_firehose(
                                    &ext_state,
                                    conn_id,
                                    tab_id,
                                    tab_url,
                                    frame_url,
                                    events,
                                    dropped,
                                )
                                .await;
                                continue;
                            }
                            if msg_type == "watch_event" {
                                let wid = val.get("watchId").and_then(|v| v.as_str()).unwrap_or("");
                                let current = val.get("current").and_then(|v| v.as_str()).unwrap_or("");
                                let previous = val.get("previous").and_then(|v| v.as_str()).unwrap_or("");
                                let url = val.get("url").and_then(|v| v.as_str());
                                if !wid.is_empty()
                                    && let Err(e) = crate::ext::deliver_watch_event(
                                        &ext_state, wid, current, previous, url,
                                    )
                                    .await
                                    {
                                        crate::broker::try_log_error(
                                            "ext",
                                            "watch event delivery failed",
                                            Some(&format!("{e:#}")),
                                        );
                                    }
                                continue;
                            }
                            if msg_type == "tab_monitor_event" {
                                let tab_id = val.get("tabId").and_then(|v| v.as_i64()).unwrap_or(-1);
                                let prev_t =
                                    val.get("previousTitle").and_then(|v| v.as_str()).unwrap_or("");
                                let cur_t =
                                    val.get("currentTitle").and_then(|v| v.as_str()).unwrap_or("");
                                let url = val.get("url").and_then(|v| v.as_str());
                                if tab_id >= 0
                                    && let Err(e) = crate::ext::deliver_tab_monitor_event(
                                        &ext_state,
                                        conn_id,
                                        tab_id,
                                        prev_t,
                                        cur_t,
                                        url,
                                    )
                                    .await
                                {
                                    crate::broker::try_log_error(
                                        "ext",
                                        "tab_monitor event delivery failed",
                                        Some(&format!("{e:#}")),
                                    );
                                }
                                continue;
                            }
                            if msg_type == "cli_exec" {
                                let id = val
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                if id.is_empty() {
                                    continue;
                                }
                                let cmd = val
                                    .get("command")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let text = val
                                    .get("text")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let bridge_tx = {
                                    let s = ext_state.lock().await;
                                    s.connections
                                        .get(&conn_id)
                                        .map(|c| c.bridge_tx.clone())
                                };
                                let Some(bridge_tx) = bridge_tx else {
                                    continue;
                                };
                                let ext_st = ext_state.clone();
                                let cid = conn_id;
                                tokio::spawn(async move {
                                    crate::ext::cli_exec_begin(&ext_st, cid).await;
                                    let reply = match async {
                                        let mut ctx = crate::AppContext::new()?;
                                        let mode = match cmd.as_str() {
                                            "inserttext" => {
                                                crate::commands::dispatch(
                                                    &mut ctx,
                                                    "inserttext",
                                                    std::slice::from_ref(&text),
                                                )
                                                .await?;
                                                "cli-insertText"
                                            }
                                            "keyboard" => {
                                                crate::commands::dispatch(
                                                    &mut ctx,
                                                    "keyboard",
                                                    std::slice::from_ref(&text),
                                                )
                                                .await?;
                                                "cli-keyboard"
                                            }
                                            _ => bail!("unknown cli_exec command: {cmd}"),
                                        };
                                        Ok::<_, anyhow::Error>(mode.to_string())
                                    }
                                    .await
                                    {
                                        Ok(mode) => json!({
                                            "id": id,
                                            "ok": true,
                                            "mode": mode,
                                        }),
                                        Err(e) => json!({
                                            "id": id,
                                            "ok": false,
                                            "error": format!("{e:#}"),
                                        }),
                                    };
                                    let line = match serde_json::to_string(&reply) {
                                        Ok(mut s) => {
                                            s.push('\n');
                                            s
                                        }
                                        Err(_) => r#"{"ok":false,"error":"serialize"}"#.to_string(),
                                    };
                                    let _ = bridge_tx.send(line);
                                    crate::ext::cli_exec_end(&ext_st, cid).await;
                                });
                                continue;
                            }
                            crate::ext::resolve_pending(&ext_state, conn_id, val).await;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }

    crate::ext::disconnect_bridge_by_id(&ext_state, conn_id).await;
    crate::broker::try_log_event(
        "info",
        "ext",
        &format!("bridge disconnected (conn: {conn_id})"),
        None,
    );
}

#[cfg(test)]
mod admin_socket_tests {
    use super::serve_admin_request;
    use anyhow::{Result, anyhow};
    use rand::RngCore;
    use std::{env, ffi::OsString, fs, path::PathBuf, sync::MutexGuard};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct HomeGuard {
        _lock: MutexGuard<'static, ()>,
        old_home: Option<OsString>,
        home: PathBuf,
    }

    impl HomeGuard {
        fn new() -> Result<Self> {
            let lock = crate::test_home_lock()
                .lock()
                .map_err(|_| anyhow!("failed to lock test HOME mutex"))?;
            let old_home = env::var_os("HOME");
            let mut bytes = [0u8; 8];
            rand::rng().fill_bytes(&mut bytes);
            let home = env::temp_dir().join(format!(
                "sidekar-admin-http-test-{}",
                bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
            ));
            fs::create_dir_all(&home)?;
            unsafe { env::set_var("HOME", &home) };
            crate::prompts::enable_db_reads_for_test(true);
            Ok(Self {
                _lock: lock,
                old_home,
                home,
            })
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            crate::prompts::enable_db_reads_for_test(false);
            match &self.old_home {
                Some(home) => unsafe { env::set_var("HOME", home) },
                None => unsafe { env::remove_var("HOME") },
            }
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    /// Send one raw request to a throwaway listener wired to the admin
    /// router, and return the response text.
    async fn roundtrip(request: &str) -> Result<String> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            serve_admin_request(&mut stream, port, serde_json::Value::Null).await
        });

        let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
        let request = request.replace("{port}", &port.to_string());
        client.write_all(request.as_bytes()).await?;
        let mut response = String::new();
        client.read_to_string(&mut response).await?;
        server.await?;
        Ok(response)
    }

    fn status_line(response: &str) -> &str {
        response.lines().next().unwrap_or("")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn get_prompts_lists_every_builtin() -> Result<()> {
        let _home = HomeGuard::new()?;
        let response =
            roundtrip("GET /api/prompts HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").await?;
        assert!(status_line(&response).contains("200 OK"), "{response}");
        let body = response.split("\r\n\r\n").nth(1).unwrap_or("");
        let parsed: serde_json::Value = serde_json::from_str(body)?;
        let items = parsed["prompts"].as_array().expect("prompts array");
        assert_eq!(items.len(), crate::prompts::BUILTIN_PROMPTS.len());
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_admin_page_offers_the_prompts_section() -> Result<()> {
        let _home = HomeGuard::new()?;
        let response = roundtrip("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").await?;
        assert!(status_line(&response).contains("200 OK"), "{response}");
        assert!(response.contains("promptsBtn"));
        assert!(response.contains("/api/prompts/set"));
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn posting_a_prompt_stores_it_and_returns_the_new_list() -> Result<()> {
        let _home = HomeGuard::new()?;
        let payload = r#"{"key":"compaction.system","value":"terse summarizer"}"#;
        let request = format!(
            "POST /api/prompts/set HTTP/1.1\r\n\
             Host: 127.0.0.1:{{port}}\r\n\
             Origin: http://127.0.0.1:{{port}}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n{payload}",
            payload.len()
        );
        let response = roundtrip(&request).await?;
        assert!(status_line(&response).contains("200 OK"), "{response}");
        assert_eq!(
            crate::prompts::get(crate::prompts::KEY_COMPACTION_SYSTEM),
            "terse summarizer"
        );
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_cross_origin_post_cannot_change_a_prompt() -> Result<()> {
        let _home = HomeGuard::new()?;
        let before = crate::prompts::get(crate::prompts::KEY_COMPACTION_SYSTEM);
        let payload = r#"{"key":"compaction.system","value":"exfiltrate everything"}"#;
        let request = format!(
            "POST /api/prompts/set HTTP/1.1\r\n\
             Host: 127.0.0.1:{{port}}\r\n\
             Origin: https://evil.example.com\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n{payload}",
            payload.len()
        );
        let response = roundtrip(&request).await?;
        assert!(status_line(&response).contains("403"), "{response}");
        assert_eq!(
            crate::prompts::get(crate::prompts::KEY_COMPACTION_SYSTEM),
            before
        );
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_preflight_is_not_answered_for_api_routes() -> Result<()> {
        let _home = HomeGuard::new()?;
        let response = roundtrip(
            "OPTIONS /api/prompts/set HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\
             Origin: https://evil.example.com\r\n\
             Access-Control-Request-Method: POST\r\n\r\n",
        )
        .await?;
        assert!(status_line(&response).contains("405"), "{response}");
        assert!(
            !response.contains("Access-Control-Allow-Origin"),
            "{response}"
        );
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_oversized_body_is_rejected_before_it_is_read() -> Result<()> {
        let _home = HomeGuard::new()?;
        let request = format!(
            "POST /api/prompts/set HTTP/1.1\r\n\
             Host: 127.0.0.1:{{port}}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n",
            super::MAX_BODY_BYTES + 1
        );
        let response = roundtrip(&request).await?;
        assert!(status_line(&response).contains("413"), "{response}");
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn non_admin_paths_are_left_for_the_websocket_upgrade() -> Result<()> {
        let _home = HomeGuard::new()?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            serve_admin_request(&mut stream, port, serde_json::Value::Null).await
        });
        let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
        client
            .write_all(b"GET /ext HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await?;
        assert!(
            !server.await?,
            "/ext must not be handled by the admin router"
        );
        Ok(())
    }
}

#[cfg(test)]
mod request_head_tests {
    use super::parse_request_head;

    #[test]
    fn parses_method_path_query_and_headers() {
        let raw = "POST /api/prompts/set?x=1 HTTP/1.1\r\n\
                   Host: 127.0.0.1:21517\r\n\
                   Content-Type: application/json\r\n\
                   Content-Length: 9\r\n\
                   \r\n\
                   {\"key\":1}";
        let head = parse_request_head(raw).expect("head");
        assert_eq!(head.method, "POST");
        assert_eq!(head.path, "/api/prompts/set");
        assert_eq!(head.query, "x=1");
        assert_eq!(head.header("host"), Some("127.0.0.1:21517"));
        assert_eq!(head.header("CONTENT-TYPE"), Some("application/json"));
        assert_eq!(head.content_length, 9);
        assert_eq!(&raw[head.head_len..], "{\"key\":1}");
    }

    #[test]
    fn missing_content_length_reads_as_zero() {
        let head = parse_request_head("GET / HTTP/1.1\r\nHost: x\r\n\r\n").expect("head");
        assert_eq!(head.content_length, 0);
        assert_eq!(head.query, "");
    }

    #[test]
    fn incomplete_headers_are_rejected() {
        assert!(parse_request_head("POST /api/prompts/set HTTP/1.1\r\nHost: x").is_none());
    }

    #[test]
    fn bare_lf_request_still_parses() {
        let head = parse_request_head("GET /health HTTP/1.1\nHost: x\n\n").expect("head");
        assert_eq!(head.path, "/health");
        assert_eq!(head.header("host"), Some("x"));
    }
}

#[cfg(test)]
mod health_probe_tests {
    use super::{health_request_path, is_health_probe};

    #[test]
    fn health_path_strips_query_string() {
        assert_eq!(
            health_request_path("GET /health?foo=1 HTTP/1.1"),
            Some("/health")
        );
        assert_eq!(
            health_request_path("OPTIONS /health HTTP/1.1"),
            Some("/health")
        );
    }

    #[test]
    fn health_probe_detection() {
        assert!(is_health_probe("GET /health HTTP/1.1"));
        assert!(is_health_probe("OPTIONS /health HTTP/1.1"));
        assert!(is_health_probe("get /health HTTP/1.1"));
        assert!(!is_health_probe("GET /ext HTTP/1.1"));
        assert!(!is_health_probe("GET /healthcheck HTTP/1.1"));
    }
}
