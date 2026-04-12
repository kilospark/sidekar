use super::*;

// ---------------------------------------------------------------------------
// Reverse proxy mode — plain HTTP, no TLS on the local leg
// Agent sets ANTHROPIC_BASE_URL=http://127.0.0.1:<port>
// ---------------------------------------------------------------------------

pub(super) async fn handle_reverse_proxy_http(
    state: Arc<ProxyState>,
    stream: TcpStream,
) -> Result<()> {
    let start_time = std::time::Instant::now();
    let (client_read, mut client_write) = tokio::io::split(stream);
    let mut reader = BufReader::new(client_read);

    // Read request line: "POST /v1/messages HTTP/1.1"
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 3 {
        bail!("invalid request line");
    }
    let method = parts[0];
    let path = parts[1];
    let version = parts[2];

    // Read headers — pass through as-is, just parse for routing + content-length
    let mut raw_headers: Vec<String> = Vec::new();
    let mut parsed_headers: Vec<(String, String)> = Vec::new();
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            parsed_headers.push((k.trim().to_string(), v.trim().to_string()));
            if k.trim().to_lowercase() == "content-length" {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
        // Skip Host — rewritten after we resolve upstream (CDN requires correct Host)
        if !line.to_lowercase().starts_with("host:") {
            raw_headers.push(line);
        }
    }

    // Read request body
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).await?;
    }

    let (upstream_host, upstream_port) = resolve_upstream(&parsed_headers);

    let is_websocket = parsed_headers
        .iter()
        .any(|(k, v)| k.to_lowercase() == "upgrade" && v.to_lowercase() == "websocket");

    if state.verbose {
        let ws_tag = if is_websocket { " [WS]" } else { "" };
        crate::broker::try_log_event(
            "debug",
            "proxy",
            &format!("REVERSE {method} {path} → {upstream_host}{ws_tag} ({content_length}b)"),
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
    let (body, newlines_saved) = if content_length > 0 {
        let (squashed, saved) = squash_newlines(&body);
        if saved > 0 {
            // Update Content-Length in raw_headers
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
        (squashed, saved)
    } else {
        (body, 0)
    };
    let _ = newlines_saved; // used above

    // Connect TLS to upstream
    let upstream_tcp = TcpStream::connect((upstream_host, upstream_port)).await?;
    let server_name = rustls::pki_types::ServerName::try_from(upstream_host.to_string())?;
    let upstream_tls = state
        .tls_connector
        .connect(server_name, upstream_tcp)
        .await?;
    let (upstream_read, mut upstream_write) = tokio::io::split(upstream_tls);

    // Forward request — only Host is rewritten (CDN routing requires it)
    upstream_write
        .write_all(format!("{method} {path} {version}\r\nHost: {upstream_host}\r\n").as_bytes())
        .await?;
    for h in &raw_headers {
        upstream_write.write_all(h.as_bytes()).await?;
    }
    upstream_write.write_all(b"\r\n").await?;
    if !body.is_empty() {
        upstream_write.write_all(&body).await?;
    }
    upstream_write.flush().await?;

    if is_websocket {
        // WebSocket: bidirectional pipe — skip payload logging
        let mut client_reader = reader;
        let mut upstream_reader = upstream_read;
        tokio::select! {
            r = tokio::io::copy(&mut client_reader, &mut upstream_write) => { let _ = r; }
            r = tokio::io::copy(&mut upstream_reader, &mut client_write) => { let _ = r; }
        }
    } else {
        // HTTP: parse response status + headers, then stream body with tee
        let mut upstream_reader = BufReader::new(upstream_read);

        // Response status line
        let mut response_line = String::new();
        upstream_reader.read_line(&mut response_line).await?;
        client_write.write_all(response_line.as_bytes()).await?;
        let response_status = response_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);

        // Response headers
        let mut resp_headers: Vec<(String, String)> = Vec::new();
        loop {
            let mut line = String::new();
            upstream_reader.read_line(&mut line).await?;
            client_write.write_all(line.as_bytes()).await?;
            if line.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                resp_headers.push((k.trim().to_string(), v.trim().to_string()));
            }
        }

        // Stream body, teeing to accumulator for logging
        let mut response_buf: Vec<u8> = Vec::new();
        let mut buf = vec![0u8; 8192];
        loop {
            let n = upstream_reader.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            client_write.write_all(&buf[..n]).await?;
            client_write.flush().await?;
            if response_buf.len() + n <= MAX_RESPONSE_CAPTURE {
                response_buf.extend_from_slice(&buf[..n]);
            }
        }

        let _ = state.log_tx.send(ProxyLogEntry {
            method: method.to_string(),
            path: path.to_string(),
            upstream_host: upstream_host.to_string(),
            request_headers: parsed_headers.clone(),
            request_body: body,
            response_status,
            response_headers: resp_headers,
            response_body: response_buf,
            duration_ms: start_time.elapsed().as_millis() as u64,
        });
    }

    Ok(())
}
