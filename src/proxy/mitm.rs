use super::*;

// ---------------------------------------------------------------------------
// MITM proxy mode — agent sends CONNECT tunnel
// ---------------------------------------------------------------------------

/// Get or generate a host certificate, cached by hostname.
pub(super) async fn get_host_cert(state: &ProxyState, host: &str) -> Result<Arc<CachedCert>> {
    if let Some(cached) = state.host_cache.read().await.get(host) {
        return Ok(cached.clone());
    }

    let host_key = rcgen::KeyPair::generate()?;
    let mut host_params = rcgen::CertificateParams::new(vec![host.to_string()])?;
    host_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, host);
    host_params
        .extended_key_usages
        .push(rcgen::ExtendedKeyUsagePurpose::ServerAuth);
    let host_cert = host_params.signed_by(&host_key, &state.ca_cert, &state.ca_key)?;

    let cached = Arc::new(CachedCert {
        der: host_cert.der().clone(),
        key_der: host_key.serialize_der(),
    });

    state
        .host_cache
        .write()
        .await
        .insert(host.to_string(), cached.clone());
    Ok(cached)
}

pub(super) async fn handle_connect_proxy(state: Arc<ProxyState>, stream: TcpStream) -> Result<()> {
    let mut reader = BufReader::new(stream);

    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;

    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 3 || parts[0] != "CONNECT" {
        let inner = reader.into_inner();
        let _ = inner.try_write(b"HTTP/1.1 405 Method Not Allowed\r\n\r\n");
        return Ok(());
    }

    let authority = parts[1];
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(443)),
        None => (authority.to_string(), 443),
    };

    // Drain remaining headers
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if line.trim().is_empty() {
            break;
        }
    }

    let mut stream = reader.into_inner();
    stream
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    stream.flush().await?;

    if state.verbose {
        crate::broker::try_log_event("debug", "proxy", &format!("CONNECT {host}:{port}"), None);
    }

    let cached = get_host_cert(&state, &host).await?;

    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![cached.der.clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(cached.key_der.clone().into()),
        )?;
    let tls_acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
    let client_tls = match tls_acceptor.accept(stream).await {
        Ok(tls) => tls,
        Err(e) => {
            crate::broker::try_log_error(
                "proxy",
                &format!("tls_acceptor.accept failed for {host}:{port}: {e:?}"),
                Some(&format!("error display: {e}")),
            );
            return Err(e.into());
        }
    };

    let real_stream = TcpStream::connect(format!("{host}:{port}")).await?;
    let server_name = rustls::pki_types::ServerName::try_from(host.clone())?;
    let server_tls = state
        .tls_connector
        .connect(server_name, real_stream)
        .await?;

    let (client_read, mut client_write) = tokio::io::split(client_tls);
    let (server_read, mut server_write) = tokio::io::split(server_tls);
    let mut client_reader = BufReader::new(client_read);
    let mut server_reader = BufReader::new(server_read);

    // Parse and forward HTTP requests inside the decrypted tunnel.
    loop {
        let req_start = std::time::Instant::now();

        // Read request line from client
        let mut request_line = String::new();
        match client_reader.read_line(&mut request_line).await {
            Ok(0) | Err(_) => break,
            _ => {}
        }
        let parts: Vec<&str> = request_line.split_whitespace().collect();
        if parts.len() < 3 {
            break;
        }
        let method = parts[0];
        let path = parts[1];
        let version = parts[2];

        // Read headers
        let mut raw_headers: Vec<String> = Vec::new();
        let mut parsed_headers: Vec<(String, String)> = Vec::new();
        let mut content_length: usize = 0;
        let mut is_websocket = false;
        loop {
            let mut line = String::new();
            match client_reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                _ => {}
            }
            if line.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                let kl = k.trim().to_lowercase();
                parsed_headers.push((k.trim().to_string(), v.trim().to_string()));
                if kl == "content-length" {
                    content_length = v.trim().parse().unwrap_or(0);
                }
                if kl == "upgrade" && v.trim().to_lowercase() == "websocket" {
                    is_websocket = true;
                }
            }
            raw_headers.push(line);
        }

        // Read request body
        let mut body = vec![0u8; content_length];
        if content_length > 0 && client_reader.read_exact(&mut body).await.is_err() {
            break;
        }

        if state.verbose {
            let ws_tag = if is_websocket { " [WS]" } else { "" };
            crate::broker::try_log_event(
                "debug",
                "proxy",
                &format!("CONNECT {method} {path} → {host}{ws_tag} ({content_length}b)"),
                Some(
                    &raw_headers
                        .iter()
                        .map(|h| h.trim_end())
                        .collect::<Vec<_>>()
                        .join(" | "),
                ),
            );
        }

        // Squash consecutive newlines in request body
        let body = if content_length > 0 {
            let (squashed, saved) = squash_newlines(&body);
            if saved > 0 {
                for h in raw_headers.iter_mut() {
                    if h.to_lowercase().starts_with("content-length:") {
                        *h = format!("Content-Length: {}\r\n", squashed.len());
                    }
                }
                if state.verbose {
                    crate::broker::try_log_event(
                        "debug",
                        "proxy",
                        &format!(
                            "squashed {saved}b newlines from request ({} → {})",
                            content_length,
                            squashed.len()
                        ),
                        None,
                    );
                }
            }
            squashed
        } else {
            body
        };

        // Forward to upstream
        server_write
            .write_all(format!("{method} {path} {version}\r\n").as_bytes())
            .await?;
        for h in &raw_headers {
            server_write.write_all(h.as_bytes()).await?;
        }
        server_write.write_all(b"\r\n").await?;
        if !body.is_empty() {
            server_write.write_all(&body).await?;
        }
        server_write.flush().await?;

        if is_websocket {
            // WebSocket upgrade. We still need to observe the 101 response
            // (to parse Sec-WebSocket-Extensions), then parse frames from
            // both directions while forwarding raw bytes unchanged. We
            // accumulate text payloads and emit a single proxy_log entry
            // for the whole session.
            let mut ws_resp_headers: Vec<(String, String)> = Vec::new();

            let mut permessage_deflate = false;
            let mut client_no_ctx = false;
            let mut server_no_ctx = false;

            // Read the HTTP response line.
            let mut response_line = String::new();
            match server_reader.read_line(&mut response_line).await {
                Ok(0) | Err(_) => break,
                _ => {}
            }
            client_write.write_all(response_line.as_bytes()).await?;
            let ws_status: u16 = response_line
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(0);

            // Read & forward response headers.
            loop {
                let mut line = String::new();
                match server_reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    _ => {}
                }
                client_write.write_all(line.as_bytes()).await?;
                if line.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = line.split_once(':') {
                    let kl = k.trim().to_lowercase();
                    let vt = v.trim();
                    ws_resp_headers.push((k.trim().to_string(), vt.to_string()));
                    if kl == "sec-websocket-extensions" {
                        let vl = vt.to_lowercase();
                        if vl.contains("permessage-deflate") {
                            permessage_deflate = true;
                            if vl.contains("client_no_context_takeover") {
                                client_no_ctx = true;
                            }
                            if vl.contains("server_no_context_takeover") {
                                server_no_ctx = true;
                            }
                        }
                    }
                }
            }
            client_write.flush().await?;

            if state.verbose {
                crate::broker::try_log_event(
                    "debug",
                    "proxy",
                    &format!(
                        "WS upgrade {host}{path} status={ws_status} deflate={permessage_deflate} \
                         client_no_ctx={client_no_ctx} server_no_ctx={server_no_ctx}"
                    ),
                    None,
                );
            }

            // If the handshake failed, just pipe remaining bytes raw and bail.
            if ws_status != 101 {
                tokio::select! {
                    r = tokio::io::copy(&mut client_reader, &mut server_write) => { let _ = r; }
                    r = tokio::io::copy(&mut server_reader, &mut client_write) => { let _ = r; }
                }
                let _ = state.log_tx.send(ProxyLogEntry {
                    method: "WS".to_string(),
                    path: path.to_string(),
                    upstream_host: host.clone(),
                    request_headers: parsed_headers,
                    request_body: body,
                    response_status: ws_status,
                    response_headers: ws_resp_headers,
                    response_body: Vec::new(),
                    duration_ms: req_start.elapsed().as_millis() as u64,
                });
                break;
            }

            // Frame mode. Emit one proxy_log row per *turn* rather than per
            // session: codex holds the WS open across many turns, so a
            // flush-on-close design would never show live activity. Heuristic:
            // a turn starts when the client sends a Data message, and ends on
            // (a) the next client Data, (b) 2s of server idle, or (c) session
            // close. Raw bytes are always forwarded unchanged regardless of
            // bookkeeping.
            struct WsPending {
                request_body: Vec<u8>,
                response_body: Vec<u8>,
                start: std::time::Instant,
                last_activity: std::time::Instant,
            }
            let mut pending: Option<WsPending> = None;
            // Codex response streams can have within-turn bursts several
            // seconds apart (tool calls, reasoning gaps). Keep this generous
            // so a single turn rarely splits into multiple rows.
            let idle_flush = std::time::Duration::from_secs(15);
            let mut ticker = tokio::time::interval(std::time::Duration::from_millis(500));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            let mut client_acc = super::ws::MessageAcc::new();
            let mut server_acc = super::ws::MessageAcc::new();
            let mut client_decomp = flate2::Decompress::new(false);
            let mut server_decomp = flate2::Decompress::new(false);

            macro_rules! flush_pending {
                ($p:expr) => {{
                    let p: WsPending = $p;
                    if !p.request_body.is_empty() || !p.response_body.is_empty() {
                        let _ = state.log_tx.send(ProxyLogEntry {
                            method: "WS".to_string(),
                            path: path.to_string(),
                            upstream_host: host.clone(),
                            request_headers: parsed_headers.clone(),
                            request_body: p.request_body,
                            response_status: ws_status,
                            response_headers: ws_resp_headers.clone(),
                            response_body: p.response_body,
                            duration_ms: p.start.elapsed().as_millis() as u64,
                        });
                    }
                }};
            }

            loop {
                tokio::select! {
                    r = super::ws::read_frame(&mut client_reader) => {
                        match r {
                            Ok(Some((raw, frame))) => {
                                if server_write.write_all(&raw).await.is_err() { break; }
                                if let Some(msg) = client_acc.push(frame)
                                    && let super::ws::Message::Data { opcode, compressed, payload } = msg
                                        && (opcode == super::ws::OP_TEXT || opcode == super::ws::OP_BINARY) {
                                            let bytes = if compressed && permessage_deflate {
                                                if client_no_ctx {
                                                    client_decomp = flate2::Decompress::new(false);
                                                }
                                                super::ws::inflate_with(
                                                    &mut client_decomp,
                                                    &payload,
                                                    MAX_RESPONSE_CAPTURE,
                                                ).unwrap_or(payload)
                                            } else { payload };
                                            // New turn: flush the previous pending row first.
                                            if let Some(prev) = pending.take() {
                                                flush_pending!(prev);
                                            }
                                            let now = std::time::Instant::now();
                                            let take = bytes.len().min(MAX_RESPONSE_CAPTURE);
                                            pending = Some(WsPending {
                                                request_body: bytes[..take].to_vec(),
                                                response_body: Vec::new(),
                                                start: now,
                                                last_activity: now,
                                            });
                                        }
                            }
                            Ok(None) | Err(_) => break,
                        }
                    }
                    r = super::ws::read_frame(&mut server_reader) => {
                        match r {
                            Ok(Some((raw, frame))) => {
                                if client_write.write_all(&raw).await.is_err() { break; }
                                if let Some(msg) = server_acc.push(frame)
                                    && let super::ws::Message::Data { opcode, compressed, payload } = msg
                                        && (opcode == super::ws::OP_TEXT || opcode == super::ws::OP_BINARY) {
                                            let bytes = if compressed && permessage_deflate {
                                                if server_no_ctx {
                                                    server_decomp = flate2::Decompress::new(false);
                                                }
                                                super::ws::inflate_with(
                                                    &mut server_decomp,
                                                    &payload,
                                                    MAX_RESPONSE_CAPTURE,
                                                ).unwrap_or(payload)
                                            } else { payload };
                                            // Attach to the current turn; if the server spoke
                                            // first (unusual), open an empty-request turn.
                                            let now = std::time::Instant::now();
                                            let p = pending.get_or_insert_with(|| WsPending {
                                                request_body: Vec::new(),
                                                response_body: Vec::new(),
                                                start: now,
                                                last_activity: now,
                                            });
                                            if p.response_body.len() < MAX_RESPONSE_CAPTURE {
                                                if !p.response_body.is_empty() {
                                                    p.response_body.push(b'\n');
                                                }
                                                let room = MAX_RESPONSE_CAPTURE - p.response_body.len();
                                                let take = bytes.len().min(room);
                                                p.response_body.extend_from_slice(&bytes[..take]);
                                            }
                                            p.last_activity = now;
                                        }
                            }
                            Ok(None) | Err(_) => break,
                        }
                    }
                    _ = ticker.tick() => {
                        let should_flush = pending.as_ref().is_some_and(|p| {
                            !p.response_body.is_empty()
                                && p.last_activity.elapsed() >= idle_flush
                        });
                        if should_flush {
                            let p = pending.take().unwrap();
                            flush_pending!(p);
                        }
                    }
                }
            }

            // Flush any remaining pending turn on session close.
            if let Some(p) = pending.take() {
                flush_pending!(p);
            }
            break;
        }

        // Stream response back to client, capturing for log
        let mut response_line = String::new();
        match server_reader.read_line(&mut response_line).await {
            Ok(0) | Err(_) => break,
            _ => {}
        }
        client_write.write_all(response_line.as_bytes()).await?;
        let response_status = response_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);

        // Read and forward response headers
        let mut resp_headers: Vec<(String, String)> = Vec::new();
        let mut resp_content_length: Option<usize> = None;
        let mut is_chunked = false;
        loop {
            let mut line = String::new();
            match server_reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                _ => {}
            }
            client_write.write_all(line.as_bytes()).await?;
            if line.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                let kl = k.trim().to_lowercase();
                resp_headers.push((k.trim().to_string(), v.trim().to_string()));
                if kl == "content-length" {
                    resp_content_length = v.trim().parse().ok();
                }
                if kl == "transfer-encoding" && v.trim().to_lowercase().contains("chunked") {
                    is_chunked = true;
                }
            }
        }

        // Forward response body, teeing to accumulator
        let mut response_buf: Vec<u8> = Vec::new();
        if let Some(len) = resp_content_length {
            let mut remaining = len;
            let mut buf = vec![0u8; 8192];
            while remaining > 0 {
                let to_read = remaining.min(buf.len());
                let n = match server_reader.read(&mut buf[..to_read]).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                client_write.write_all(&buf[..n]).await?;
                if response_buf.len() + n <= MAX_RESPONSE_CAPTURE {
                    response_buf.extend_from_slice(&buf[..n]);
                }
                remaining -= n;
            }
        } else if is_chunked {
            // Forward chunked encoding until terminal chunk
            loop {
                let mut chunk_line = String::new();
                match server_reader.read_line(&mut chunk_line).await {
                    Ok(0) | Err(_) => break,
                    _ => {}
                }
                client_write.write_all(chunk_line.as_bytes()).await?;
                let chunk_size = usize::from_str_radix(chunk_line.trim(), 16).unwrap_or(0);
                if chunk_size == 0 {
                    let mut trail = String::new();
                    let _ = server_reader.read_line(&mut trail).await;
                    client_write.write_all(trail.as_bytes()).await?;
                    break;
                }
                let mut remaining = chunk_size;
                let mut buf = vec![0u8; 8192];
                while remaining > 0 {
                    let to_read = remaining.min(buf.len());
                    let n = match server_reader.read(&mut buf[..to_read]).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    client_write.write_all(&buf[..n]).await?;
                    if response_buf.len() + n <= MAX_RESPONSE_CAPTURE {
                        response_buf.extend_from_slice(&buf[..n]);
                    }
                    remaining -= n;
                }
                let mut trail = String::new();
                let _ = server_reader.read_line(&mut trail).await;
                client_write.write_all(trail.as_bytes()).await?;
            }
        }
        client_write.flush().await?;

        let _ = state.log_tx.send(ProxyLogEntry {
            method: method.to_string(),
            path: path.to_string(),
            upstream_host: host.clone(),
            request_headers: parsed_headers,
            request_body: body,
            response_status,
            response_headers: resp_headers,
            response_body: response_buf,
            duration_ms: req_start.elapsed().as_millis() as u64,
        });
    }

    Ok(())
}
