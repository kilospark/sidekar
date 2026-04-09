use super::*;

pub(super) type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub(super) async fn ws_connect_and_register(params: &ConnectParams) -> Result<(WsStream, String)> {
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
        proto: 2,
        cols: params.cols,
        rows: params.rows,
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
                        return resp.session_id.ok_or_else(|| {
                            anyhow::anyhow!("registered response missing session_id")
                        });
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

pub(super) async fn tunnel_task(
    ws: WsStream,
    session_id: Arc<Mutex<String>>,
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
                Ok((stream, new_session_id)) => {
                    if let Ok(mut g) = session_id.lock() {
                        *g = new_session_id;
                    }
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
                    Some(TunnelCommand::BusText(json)) => {
                        if ws_sink.send(Message::Text(json.into())).await.is_err() {
                            return false;
                        }
                    }
                    Some(TunnelCommand::PtyText(json)) => {
                        if ws_sink.send(Message::Text(json.into())).await.is_err() {
                            return false;
                        }
                    }
                    Some(TunnelCommand::EventText(json)) => {
                        if ws_sink.send(Message::Text(json.into())).await.is_err() {
                            return false;
                        }
                    }
                    Some(TunnelCommand::Shutdown) | None => {
                        let _ = ws_sink.close().await;
                        return true;
                    }
                }
            }

            // Relay → PTY input (binary = viewer keystrokes; text = bus multiplex)
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        let _ = evt_tx.try_send(TunnelEvent::Data(data.into()));
                    }
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            if v.get("ch").and_then(|x| x.as_str()) == Some("bus") {
                                let body_str = v
                                    .get("body")
                                    .and_then(|b| b.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let envelope = v
                                    .get("envelope_json")
                                    .and_then(|x| x.as_str())
                                    .and_then(|s| serde_json::from_str::<crate::message::Envelope>(s).ok());
                                if let (Some(recipient), Some(sender)) = (
                                    v.get("recipient").and_then(|x| x.as_str()),
                                    v.get("sender").and_then(|x| x.as_str()),
                                ) {
                                    let _ = evt_tx.try_send(TunnelEvent::BusRelay {
                                        recipient: recipient.to_string(),
                                        sender: sender.to_string(),
                                        body: body_str,
                                        envelope,
                                    });
                                } else {
                                    let _ = evt_tx.try_send(TunnelEvent::BusPlain(body_str));
                                }
                            }
                        }
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

pub(super) fn gethostname() -> String {
    let mut buf = [0u8; 256];
    let ret = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if ret == 0 {
        let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..len]).to_string()
    } else {
        "unknown".to_string()
    }
}
