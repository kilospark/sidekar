use super::*;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::protocol::Message;

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
    eprintln!("sidekar: could not bind HTTP listener on ports {HTTP_PORT_START}-{HTTP_PORT_END}");
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
                eprintln!("HTTP accept error: {e}");
            }
        }
    }
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

    if first_line.starts_with("GET /health") {
        let body = r#"{"sidekar":true}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: application/json\r\n\
             x-sidekar: 1\r\n\
             Access-Control-Allow-Origin: *\r\n\
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

    if first_line.contains("/ext") {
        let ext_state = state.lock().await.ext_state.clone();
        match tokio_tungstenite::accept_async(stream).await {
            Ok(ws) => handle_ext_websocket(ws, ext_state).await,
            Err(e) => {
                if crate::runtime::verbose() {
                    eprintln!("WS handshake failed: {e}");
                }
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

    let (ext_token, agent_id, browser_name) = loop {
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
                    break (token, aid, browser);
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

    let (conn_id, mut bridge_rx, profile) =
        crate::ext::register_bridge_ws(&ext_state, user_id.clone(), agent_id, browser_name.clone())
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

    eprintln!(
        "[sidekar] Extension bridge connected via WebSocket (conn: {conn_id}, browser: {browser_name}, user: {user_id})"
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
                eprintln!("[sidekar] Extension WS keepalive timeout (conn {ka_conn_id})");
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
                                        eprintln!("[sidekar] watch event delivery failed: {e}");
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
    eprintln!("[sidekar] Extension WS bridge disconnected (conn: {conn_id})");
}
