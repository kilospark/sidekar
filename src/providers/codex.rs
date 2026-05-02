use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::sync::mpsc;

use super::{
    AssistantResponse, ChatMessage, ContentBlock, RateLimitSnapshot, Role, StopReason, StreamEvent,
    ToolDef, Usage,
};

// ---------------------------------------------------------------------------
// Persistent WebSocket connection
// ---------------------------------------------------------------------------

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsWrite = futures_util::stream::SplitSink<WsStream, tokio_tungstenite::tungstenite::Message>;
type WsRead = futures_util::stream::SplitStream<WsStream>;

fn log_ws_verbose(event: &str, detail: Option<&str>) {
    if !super::is_verbose() {
        return;
    }
    crate::broker::try_log_event("debug", "codex-ws", event, detail);
}

fn log_ws_error(event: &str, detail: &str) {
    crate::broker::try_log_error("codex-ws", event, Some(detail));
}

/// A reusable WebSocket connection to the Codex Responses API.
///
/// Held across turns in a REPL session so the server can correlate requests
/// and cache prompt prefixes per-connection — matching codex CLI behavior.
pub struct CachedWs {
    write: WsWrite,
    read: WsRead,
}

/// Non-streaming call to the OpenAI Codex Responses API.
#[allow(clippy::too_many_arguments)]
pub async fn stream(
    api_key: &str,
    account_id: &str,
    base_url: &str,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    prompt_cache_key: Option<&str>,
    previous_response_id: Option<&str>,
    config: &super::StreamConfig,
) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
    let body = build_request_body(
        model,
        system_prompt,
        messages,
        tools,
        prompt_cache_key,
        previous_response_id,
        config,
    );
    let url = format!("{}/codex/responses", base_url.trim_end_matches('/'));

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("content-type", "application/json".parse()?);
    headers.insert("authorization", format!("Bearer {api_key}").parse()?);
    headers.insert("OpenAI-Beta", "responses=experimental".parse()?);
    headers.insert("originator", "sidekar".parse()?);

    if !account_id.is_empty() {
        headers.insert("chatgpt-account-id", account_id.parse()?);
    }

    super::log_api_request(&url, &headers, &body);

    let client = super::build_streaming_client(std::time::Duration::from_secs(300))?;

    let response = client
        .post(&url)
        .headers(headers)
        .json(&body)
        .send()
        .await
        .context("failed to connect to Codex API")?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        super::log_api_error(status, &text);
        bail!("Codex API error ({}): {}", status, text);
    }

    let rate_limit = {
        let snap = RateLimitSnapshot::from_openai_headers(response.headers());
        if snap.is_empty() { None } else { Some(snap) }
    };

    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        if let Err(e) = parse_sse_stream(response, rate_limit, &tx).await {
            let _ = tx.send(StreamEvent::Error {
                message: format!("{e:#}"),
            });
        }
    });

    Ok(rx)
}

// ---------------------------------------------------------------------------
// Request body
// ---------------------------------------------------------------------------

fn build_request_body(
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    prompt_cache_key: Option<&str>,
    previous_response_id: Option<&str>,
    config: &super::StreamConfig,
) -> Value {
    let mut input: Vec<Value> = Vec::new();

    for msg in messages {
        match msg.role {
            Role::User => {
                let mut pending_parts: Vec<Value> = Vec::new();

                fn flush_codex_user_message(input: &mut Vec<Value>, parts: &mut Vec<Value>) {
                    if parts.is_empty() {
                        return;
                    }
                    let only_plain_text = parts.len() == 1
                        && parts[0].get("type").and_then(|v| v.as_str()) == Some("input_text");
                    if only_plain_text {
                        let t = parts[0].get("text").and_then(|v| v.as_str()).unwrap_or("");
                        input.push(json!({
                            "type": "message",
                            "role": "user",
                            "content": t,
                        }));
                    } else {
                        input.push(json!({
                            "type": "message",
                            "role": "user",
                            "content": parts.clone(),
                        }));
                    }
                    parts.clear();
                }

                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => {
                            if !text.is_empty() {
                                pending_parts.push(json!({
                                    "type": "input_text",
                                    "text": text,
                                }));
                            }
                        }
                        ContentBlock::Image {
                            media_type,
                            data_base64,
                            ..
                        } => {
                            let url = format!("data:{media_type};base64,{data_base64}");
                            pending_parts.push(json!({
                                "type": "input_image",
                                "image_url": url,
                            }));
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            flush_codex_user_message(&mut input, &mut pending_parts);
                            let (call_id, _) = split_tool_call_ids(tool_use_id);
                            input.push(json!({
                                "type": "function_call_output",
                                "call_id": call_id,
                                "output": content,
                            }));
                        }
                        _ => {}
                    }
                }
                flush_codex_user_message(&mut input, &mut pending_parts);
            }
            Role::Assistant => {
                // Encrypted reasoning blobs — must precede text/tool_call
                // items so the server can reconstruct its reasoning chain
                // before the output that followed.
                for block in &msg.content {
                    if let ContentBlock::EncryptedReasoning {
                        encrypted_content,
                        summary,
                    } = block
                    {
                        input.push(json!({
                            "type": "reasoning",
                            "encrypted_content": encrypted_content,
                            "summary": summary,
                        }));
                    }
                }

                // Text output
                let text = super::openai_compat_assistant_join_text(&msg.content);

                if !text.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": text,
                    }));
                }

                // Tool calls
                for block in &msg.content {
                    if let ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                        ..
                    } = block
                    {
                        let (call_id, item_id) = split_tool_call_ids(id);
                        input.push(json!({
                            "type": "function_call",
                            "id": item_id,
                            "call_id": call_id,
                            "name": name,
                            "arguments": arguments.to_string(),
                        }));
                    }
                }
            }
        }
    }

    let api_tools: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "name": t.name,
                "description": t.description,
                "parameters": t.input_schema,
            })
        })
        .collect();

    // Codex's backend requires `store: false` for OAuth-auth'd calls. With
    // store disabled, the server drops reasoning context between turns
    // unless we explicitly ask for encrypted reasoning to be echoed back —
    // `include: ["reasoning.encrypted_content"]` is what the codex CLI sends.
    let mut body = json!({
        "model": model,
        "instructions": system_prompt,
        "input": input,
        "stream": true,
        "store": false,
        "include": ["reasoning.encrypted_content"],
    });

    // previous_response_id is NOT compatible with store:false — the server
    // returns "Previous response not found" because it doesn't persist.
    // Keeping the plumbing in case store:true becomes viable.
    if let Some(_rid) = previous_response_id.filter(|id| !id.is_empty()) {
        // body["previous_response_id"] = json!(rid);
    }

    if let Some(key) = prompt_cache_key.filter(|key| !key.is_empty()) {
        body["prompt_cache_key"] = json!(key);
    }

    if !api_tools.is_empty() {
        body["tools"] = json!(api_tools);
        body["tool_choice"] = json!("auto");
        if config.parallel_tool_calls {
            body["parallel_tool_calls"] = json!(true);
        }
    }

    if let Some(temp) = config.temperature {
        body["temperature"] = json!(temp);
    }

    if let Some(ref reasoning) = config.reasoning {
        body["reasoning"] = json!({
            "effort": reasoning.effort,
            "summary": reasoning.summary,
        });
    }

    if let Some(ref verbosity) = config.text_verbosity {
        body["text"] = json!({ "verbosity": verbosity });
    }

    body
}

// ---------------------------------------------------------------------------
// SSE stream parsing
// ---------------------------------------------------------------------------

async fn parse_sse_stream(
    response: reqwest::Response,
    rate_limit: Option<RateLimitSnapshot>,
    tx: &mpsc::UnboundedSender<StreamEvent>,
) -> Result<()> {
    let mut stream = response.bytes_stream();
    let mut decoder = super::SseDecoder::new();

    let mut content_blocks: Vec<ContentBlock> = Vec::new();
    let mut usage = Usage::default();
    let mut model_id = String::new();
    let mut response_id = String::new();
    let mut has_tool_calls = false;
    let mut next_tool_index = 0usize;
    let mut pending_tool_calls: HashMap<String, PendingToolCall> = HashMap::new();

    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error reading SSE chunk")?;
        decoder.push_chunk(&chunk);

        while let Some(event) = decoder.next_event() {
            let data: Value = match super::parse_sse_json(&event) {
                Some(v) => v,
                None => continue,
            };

            let event_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match event_type {
                "response.created" => {
                    model_id = data
                        .get("response")
                        .and_then(|r| r.get("model"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                }

                "response.output_item.added" => {
                    let item = data.get("item").unwrap_or(&Value::Null);
                    let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");

                    if item_type == "function_call" {
                        has_tool_calls = true;
                        let item_id = item
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let call_id = item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let partial_json = item
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let index = next_tool_index;
                        next_tool_index += 1;
                        let _ = tx.send(StreamEvent::ToolCallStart {
                            index,
                            id: call_id.clone(),
                            name: name.clone(),
                        });
                        if !partial_json.is_empty() {
                            let _ = tx.send(StreamEvent::ToolCallDelta {
                                index,
                                delta: partial_json.clone(),
                            });
                        }
                        pending_tool_calls.insert(
                            item_id,
                            PendingToolCall {
                                call_id,
                                index,
                                partial_json,
                                name: name.clone(),
                            },
                        );
                    }
                }

                "response.output_text.delta" => {
                    if let Some(delta) = data.get("delta").and_then(|v| v.as_str())
                        && !delta.is_empty()
                    {
                        let _ = tx.send(StreamEvent::TextDelta {
                            delta: delta.to_string(),
                        });
                    }
                }

                "response.function_call_arguments.delta" => {
                    if let Some(delta) = data.get("delta").and_then(|v| v.as_str()) {
                        let index = if let Some(call) =
                            get_pending_tool_call_mut(&mut pending_tool_calls, &data)
                        {
                            call.partial_json.push_str(delta);
                            call.index
                        } else {
                            0
                        };
                        let _ = tx.send(StreamEvent::ToolCallDelta {
                            index,
                            delta: delta.to_string(),
                        });
                    }
                }

                "response.function_call_arguments.done" => {
                    if let Some(call) = get_pending_tool_call_mut(&mut pending_tool_calls, &data) {
                        call.partial_json = data
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&call.partial_json)
                            .to_string();
                    }
                }

                "response.output_text.done" => {
                    if let Some(text) = data.get("text").and_then(|v| v.as_str())
                        && !text.is_empty()
                    {
                        content_blocks.push(ContentBlock::Text {
                            text: text.to_string(),
                        });
                    }
                }

                "response.output_item.done" => {
                    let item = data.get("item").unwrap_or(&Value::Null);
                    let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");

                    if item_type == "function_call" {
                        let item_id = item
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let pending = pending_tool_calls.remove(&item_id);
                        let index = pending.as_ref().map(|call| call.index).unwrap_or(0);
                        let call_id = item
                            .get("call_id")
                            .and_then(|v| v.as_str())
                            .filter(|id| !id.is_empty())
                            .map(str::to_string)
                            .or_else(|| pending.as_ref().map(|call| call.call_id.clone()))
                            .unwrap_or_else(|| item_id.clone());
                        let name = item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .filter(|name| !name.is_empty())
                            .map(str::to_string)
                            .or_else(|| pending.as_ref().map(|call| call.name.clone()))
                            .unwrap_or_default();
                        let args_str = item
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .filter(|args| !args.is_empty())
                            .map(str::to_string)
                            .or_else(|| pending.as_ref().map(|call| call.partial_json.clone()))
                            .unwrap_or_else(|| "{}".to_string());
                        let arguments: Value = serde_json::from_str(&args_str).unwrap_or(json!({}));
                        // Always store both IDs so we can reconstruct the request
                        let stored_id = if item_id.is_empty() || item_id == call_id {
                            call_id.clone()
                        } else {
                            format!("{call_id}|{item_id}")
                        };

                        let _ = tx.send(StreamEvent::ToolCallEnd { index });
                        content_blocks.push(ContentBlock::ToolCall {
                            id: stored_id,
                            name,
                            arguments,
                            thought_signature: None,
                        });
                    } else if item_type == "reasoning" {
                        // Capture encrypted reasoning blob for round-tripping.
                        if let Some(enc) = item.get("encrypted_content").and_then(|v| v.as_str()) {
                            let summary = item
                                .get("summary")
                                .and_then(|v| v.as_array())
                                .cloned()
                                .unwrap_or_default();
                            content_blocks.push(ContentBlock::EncryptedReasoning {
                                encrypted_content: enc.to_string(),
                                summary,
                            });
                        }
                    }
                }

                "response.completed" | "response.done" | "response.incomplete" => {
                    if let Some(resp) = data.get("response") {
                        if let Some(u) = resp.get("usage") {
                            apply_usage(u, &mut usage);
                        }
                        let m = resp.get("model").and_then(|v| v.as_str()).unwrap_or("");
                        if !m.is_empty() {
                            model_id = m.to_string();
                        }
                        let rid = resp.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        if !rid.is_empty() {
                            response_id = rid.to_string();
                        }
                    }

                    // Extract encrypted reasoning from response.output[]
                    // (may not have arrived via individual output_item.done events).
                    extract_reasoning_from_completed(&data, &mut content_blocks);

                    let stop = if has_tool_calls {
                        StopReason::ToolUse
                    } else {
                        StopReason::Stop
                    };
                    let _ = tx.send(StreamEvent::Done {
                        message: AssistantResponse {
                            content: std::mem::take(&mut content_blocks),
                            usage: usage.clone(),
                            stop_reason: stop,
                            model: model_id.clone(),
                            response_id: response_id.clone(),
                            rate_limit: rate_limit.clone(),
                        },
                    });
                }

                "response.failed" => {
                    let msg = data
                        .get("response")
                        .and_then(|r| r.get("error"))
                        .and_then(|e| e.get("message"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("Codex request failed");
                    let _ = tx.send(StreamEvent::Error {
                        message: msg.to_string(),
                    });
                }

                "error" => {
                    let msg = data
                        .get("message")
                        .and_then(|v| v.as_str())
                        .or_else(|| {
                            data.get("error")
                                .and_then(|e| e.get("message"))
                                .and_then(|v| v.as_str())
                        })
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("Codex SSE error: {data}"));
                    let _ = tx.send(StreamEvent::Error { message: msg });
                }

                _ => {} // Ignore other event types (ping, etc.)
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct PendingToolCall {
    call_id: String,
    index: usize,
    partial_json: String,
    name: String,
}

/// Extract encrypted reasoning items from `response.output[]` in the
/// `response.completed` event.  The WS/SSE stream may NOT send individual
/// `response.output_item.done` events for reasoning items — they only
/// appear in the final completed payload's `output` array.
fn extract_reasoning_from_completed(data: &Value, content_blocks: &mut Vec<ContentBlock>) {
    let output = data
        .get("response")
        .and_then(|r| r.get("output"))
        .and_then(|v| v.as_array());
    if let Some(items) = output {
        for item in items {
            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if item_type == "reasoning"
                && let Some(enc) = item.get("encrypted_content").and_then(|v| v.as_str())
            {
                // Only add if we didn't already capture it from
                // a response.output_item.done event.
                let already_have = content_blocks.iter().any(|b| {
                    matches!(b, ContentBlock::EncryptedReasoning { encrypted_content, .. }
                            if encrypted_content == enc)
                });
                if !already_have {
                    let summary = item
                        .get("summary")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    content_blocks.push(ContentBlock::EncryptedReasoning {
                        encrypted_content: enc.to_string(),
                        summary,
                    });
                }
            }
        }
    }
}

fn get_event_call_id(data: &Value) -> Option<String> {
    data.get("item_id")
        .or_else(|| data.get("call_id"))
        .or_else(|| data.get("item").and_then(|item| item.get("call_id")))
        .or_else(|| data.get("item").and_then(|item| item.get("id")))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn get_pending_tool_call_mut<'a>(
    pending_tool_calls: &'a mut HashMap<String, PendingToolCall>,
    data: &Value,
) -> Option<&'a mut PendingToolCall> {
    if let Some(event_id) = get_event_call_id(data)
        && pending_tool_calls.contains_key(&event_id)
    {
        return pending_tool_calls.get_mut(&event_id);
    }

    if pending_tool_calls.len() == 1 {
        let only_key = pending_tool_calls.keys().next()?.to_string();
        return pending_tool_calls.get_mut(&only_key);
    }

    None
}

/// Parse PEM-encoded certificates without the `rustls_pemfile` crate.
fn parse_pem_certs(pem: &[u8]) -> Vec<rustls::pki_types::CertificateDer<'static>> {
    use base64::Engine;
    let text = String::from_utf8_lossy(pem);
    let mut certs = Vec::new();
    let mut in_cert = false;
    let mut b64 = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "-----BEGIN CERTIFICATE-----" {
            in_cert = true;
            b64.clear();
        } else if trimmed == "-----END CERTIFICATE-----" {
            in_cert = false;
            if let Ok(der) = base64::engine::general_purpose::STANDARD.decode(&b64) {
                certs.push(rustls::pki_types::CertificateDer::from(der));
            }
        } else if in_cert {
            b64.push_str(trimmed);
        }
    }
    certs
}

fn split_tool_call_ids(stored_id: &str) -> (String, String) {
    if let Some((call_id, item_id)) = stored_id.split_once('|') {
        return (call_id.to_string(), item_id.to_string());
    }

    // Native Codex format
    if let Some(suffix) = stored_id.strip_prefix("call_") {
        return (stored_id.to_string(), format!("fc_{suffix}"));
    }
    if let Some(suffix) = stored_id.strip_prefix("fc_") {
        return (format!("call_{suffix}"), stored_id.to_string());
    }

    // Foreign ID (e.g. Anthropic's toolu_*) — generate Codex-compatible IDs
    let hash = format!("{:x}", xxhash_rust::xxh64::xxh64(stored_id.as_bytes(), 0));
    let short = &hash[..hash.len().min(12)];
    (format!("call_{short}"), format!("fc_{short}"))
}

fn apply_usage(u: &Value, usage: &mut Usage) {
    let input_total = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    usage.output_tokens = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

    let details = u.get("input_tokens_details");
    usage.cache_read_tokens = details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    usage.cache_write_tokens = details
        .and_then(|d| d.get("cache_creation_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    usage.input_tokens = input_total
        .saturating_sub(usage.cache_read_tokens)
        .saturating_sub(usage.cache_write_tokens);
}

// ---------------------------------------------------------------------------
// WebSocket transport
// ---------------------------------------------------------------------------

/// Open a fresh WebSocket connection to the Codex Responses API.
///
/// Handles both direct and MITM-proxy paths. Returns split write+read halves.
async fn connect_ws(
    api_key: &str,
    account_id: &str,
    base_url: &str,
    verbose: bool,
) -> Result<(WsWrite, WsRead)> {
    let http_url = format!("{}/codex/responses", base_url.trim_end_matches('/'));
    let ws_url = http_url
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);

    use tokio_tungstenite::tungstenite::http::Request;
    let ws_key = tokio_tungstenite::tungstenite::handshake::client::generate_key();
    let host = ws_url
        .split("://")
        .nth(1)
        .and_then(|s| s.split('/').next())
        .unwrap_or("chatgpt.com");
    let mut req_builder = Request::builder()
        .uri(&ws_url)
        .header("Host", host)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", &ws_key)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("OpenAI-Beta", "responses_websockets=2026-02-06")
        .header("originator", "sidekar");
    if !account_id.is_empty() {
        req_builder = req_builder.header("chatgpt-account-id", account_id);
    }
    let ws_request = req_builder.body(()).context("failed to build WS request")?;

    // Build rustls TLS config (explicit ring provider)
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some((_port, ref ca_pem)) = super::attached_mitm_for_custom_tls() {
        for cert in parse_pem_certs(ca_pem) {
            let _ = roots.add(cert);
        }
    }
    let tls_config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("ring TLS versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    let tls_config = std::sync::Arc::new(tls_config);

    let ws = if let Some((proxy_port, _)) = super::attached_mitm_for_custom_tls() {
        let proxy_addr = format!("127.0.0.1:{}", proxy_port);
        if verbose {
            log_ws_verbose(
                "connect-tunnel-via-proxy",
                Some(&format!("port={proxy_port}")),
            );
        }
        let mut tcp = tokio::net::TcpStream::connect(&proxy_addr)
            .await
            .context("failed to connect to MITM proxy for WS")?;

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let connect_req = format!("CONNECT {host}:443 HTTP/1.1\r\nHost: {host}:443\r\n\r\n");
        tcp.write_all(connect_req.as_bytes()).await?;
        tcp.flush().await?;

        let mut resp_buf = Vec::with_capacity(256);
        loop {
            let mut b = [0u8; 1];
            match tcp.read(&mut b).await {
                Ok(0) | Err(_) => anyhow::bail!("proxy closed during CONNECT"),
                Ok(_) => resp_buf.push(b[0]),
            }
            if resp_buf.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        if !resp_buf.starts_with(b"HTTP/1.1 200") {
            anyhow::bail!(
                "proxy CONNECT failed: {}",
                String::from_utf8_lossy(&resp_buf)
            );
        }

        if verbose {
            log_ws_verbose(
                "tls-ws-handshake",
                Some(&format!("host={host} via_proxy=true")),
            );
        }
        let connector = Some(tokio_tungstenite::Connector::Rustls(tls_config));
        let (ws, _) =
            tokio_tungstenite::client_async_tls_with_config(ws_request, tcp, None, connector)
                .await
                .context("WS handshake over proxy tunnel failed")?;
        ws
    } else {
        if verbose {
            log_ws_verbose(
                "tls-ws-handshake",
                Some(&format!("host={host} via_proxy=false")),
            );
        }
        let connector = tokio_tungstenite::Connector::Rustls(tls_config);
        let (ws, _) = tokio_tungstenite::connect_async_tls_with_config(
            ws_request,
            None,
            false,
            Some(connector),
        )
        .await
        .context("failed to connect WebSocket to Codex API")?;
        ws
    };
    if verbose {
        log_ws_verbose("connected", None);
    }

    Ok(futures_util::StreamExt::split(ws))
}

/// Stream a codex response over WebSocket instead of SSE.
///
/// Same payload as the HTTP POST path, but:
/// - Protocol: `wss://` instead of `https://`
/// - Header: `OpenAI-Beta: responses_websockets=2026-02-06`
/// - Client sends: `{ "type": "response.create", ...body }`
/// - Server sends: raw JSON events per WS message (no SSE framing)
///
/// When `cached_ws` is provided, reuses the existing connection. If the send
/// fails (stale connection), transparently reconnects. After `response.completed`,
/// the connection is returned via the oneshot for the next turn to reuse.
#[allow(clippy::too_many_arguments)]
pub async fn stream_ws(
    api_key: &str,
    account_id: &str,
    base_url: &str,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    prompt_cache_key: Option<&str>,
    previous_response_id: Option<&str>,
    config: &super::StreamConfig,
    cached_ws: Option<CachedWs>,
) -> Result<(
    mpsc::UnboundedReceiver<StreamEvent>,
    tokio::sync::oneshot::Receiver<Option<CachedWs>>,
)> {
    let body = build_request_body(
        model,
        system_prompt,
        messages,
        tools,
        prompt_cache_key,
        previous_response_id,
        config,
    );

    let mut ws_body = body;
    ws_body["type"] = json!("response.create");
    let payload = serde_json::to_string(&ws_body)?;

    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let verbose = super::is_verbose();

    // Reuse cached connection or open a fresh one.
    //
    // For cached connections we validate by reading the first message: if the
    // server closed the WS while we were idle (common between user turns),
    // the send may appear to succeed (data goes to OS buffer) but the first
    // read will fail with broken pipe. Reading one message before spawning
    // the reader task lets us detect this and reconnect transparently.
    let (write, mut read, first_text) = 'conn: {
        if let Some(ws) = cached_ws {
            let (mut w, mut r) = (ws.write, ws.read);
            if verbose {
                log_ws_verbose("sending-on-cached-connection", None);
            }
            if w.send(WsMessage::Text(payload.clone().into()))
                .await
                .is_ok()
            {
                if verbose {
                    log_ws_verbose("validating-cached-connection", None);
                }
                // Validate: read first message to confirm connection is alive
                use futures_util::StreamExt;
                if let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t))) = r.next().await {
                    if verbose {
                        log_ws_verbose("cached-connection-reused", None);
                    }
                    break 'conn (w, r, Some(t.to_string()));
                }
                if verbose {
                    log_ws_verbose("cached-read-failed-reconnecting", None);
                }
            } else if verbose {
                log_ws_verbose("cached-send-failed-reconnecting", None);
            }
        } else if verbose {
            log_ws_verbose("no-cached-connection", None);
        }

        // Fresh connection (either no cache, or cache was dead)
        if verbose {
            log_ws_verbose("opening-fresh-connection", None);
        }
        let (mut w, r) = connect_ws(api_key, account_id, base_url, verbose).await?;
        if verbose {
            log_ws_verbose("sending-response-create", None);
        }
        w.send(WsMessage::Text(payload.into()))
            .await
            .context("failed to send response.create over WS")?;
        (w, r, None)
    };

    let (tx, rx) = mpsc::unbounded_channel();
    let (reclaim_tx, reclaim_rx) = tokio::sync::oneshot::channel();

    let verbose = super::is_verbose();
    tokio::spawn(async move {
        match parse_ws_stream(&mut read, &tx, first_text).await {
            Ok(true) => {
                if verbose {
                    log_ws_verbose("reclaiming-connection-for-reuse", None);
                }
                let _ = reclaim_tx.send(Some(CachedWs { write, read }));
            }
            Ok(false) => {
                if verbose {
                    log_ws_verbose("server-closed-connection", None);
                }
                let _ = reclaim_tx.send(None);
            }
            Err(e) => {
                if verbose {
                    log_ws_error("transport-error", &format!("{e:#}"));
                }
                let _ = tx.send(StreamEvent::Error {
                    message: format!("{e:#}"),
                });
                let _ = reclaim_tx.send(None);
            }
        }
    });

    Ok((rx, reclaim_rx))
}

/// Returns `Ok(true)` if the response completed and the connection is alive
/// (reusable), `Ok(false)` if the server closed the connection, or `Err` on
/// transport failure.
///
/// `first_text` is an optional pre-read message from connection validation
/// (used when reusing a cached WS — we read the first message before spawning
/// the reader task to detect broken connections).
async fn parse_ws_stream<S>(
    read: &mut futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
    tx: &mpsc::UnboundedSender<StreamEvent>,
    first_text: Option<String>,
) -> Result<bool>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let mut content_blocks: Vec<ContentBlock> = Vec::new();
    let mut usage = Usage::default();
    let mut model_id = String::new();
    let mut response_id = String::new();
    let mut has_tool_calls = false;
    let mut next_tool_index = 0usize;
    let mut pending_tool_calls: HashMap<String, PendingToolCall> = HashMap::new();
    let mut completed = false;
    let mut buffered_text = first_text;

    loop {
        let text = if let Some(t) = buffered_text.take() {
            t
        } else {
            let msg = match read.next().await {
                Some(m) => m.context("WS read error")?,
                None => break,
            };
            match msg {
                WsMessage::Text(t) => t.to_string(),
                WsMessage::Close(_) => return Ok(false),
                WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_) => continue,
                WsMessage::Binary(_) => continue,
            }
        };

        let data: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");

        // Same event dispatch as parse_sse_stream
        match event_type {
            "response.created" => {
                model_id = data
                    .get("response")
                    .and_then(|r| r.get("model"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            }

            "response.output_item.added" => {
                let item = data.get("item").unwrap_or(&Value::Null);
                let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");

                if item_type == "function_call" {
                    has_tool_calls = true;
                    let item_id = item
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let call_id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let partial_json = item
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let index = next_tool_index;
                    next_tool_index += 1;
                    let _ = tx.send(StreamEvent::ToolCallStart {
                        index,
                        id: call_id.clone(),
                        name: name.clone(),
                    });
                    if !partial_json.is_empty() {
                        let _ = tx.send(StreamEvent::ToolCallDelta {
                            index,
                            delta: partial_json.clone(),
                        });
                    }
                    pending_tool_calls.insert(
                        item_id,
                        PendingToolCall {
                            call_id,
                            index,
                            partial_json,
                            name,
                        },
                    );
                }
            }

            "response.output_text.delta" => {
                if let Some(delta) = data.get("delta").and_then(|v| v.as_str())
                    && !delta.is_empty()
                {
                    let _ = tx.send(StreamEvent::TextDelta {
                        delta: delta.to_string(),
                    });
                }
            }

            "response.function_call_arguments.delta" => {
                if let Some(delta) = data.get("delta").and_then(|v| v.as_str()) {
                    let index = if let Some(call) =
                        get_pending_tool_call_mut(&mut pending_tool_calls, &data)
                    {
                        call.partial_json.push_str(delta);
                        call.index
                    } else {
                        0
                    };
                    let _ = tx.send(StreamEvent::ToolCallDelta {
                        index,
                        delta: delta.to_string(),
                    });
                }
            }

            "response.function_call_arguments.done" => {
                if let Some(call) = get_pending_tool_call_mut(&mut pending_tool_calls, &data) {
                    call.partial_json = data
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&call.partial_json)
                        .to_string();
                }
            }

            "response.output_text.done" => {
                if let Some(text) = data.get("text").and_then(|v| v.as_str())
                    && !text.is_empty()
                {
                    content_blocks.push(ContentBlock::Text {
                        text: text.to_string(),
                    });
                }
            }

            "response.output_item.done" => {
                let item = data.get("item").unwrap_or(&Value::Null);
                let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");

                if item_type == "function_call" {
                    let item_id = item
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let pending = pending_tool_calls.remove(&item_id);
                    let index = pending.as_ref().map(|call| call.index).unwrap_or(0);
                    let call_id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .filter(|id| !id.is_empty())
                        .map(str::to_string)
                        .or_else(|| pending.as_ref().map(|call| call.call_id.clone()))
                        .unwrap_or_else(|| item_id.clone());
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .filter(|name| !name.is_empty())
                        .map(str::to_string)
                        .or_else(|| pending.as_ref().map(|call| call.name.clone()))
                        .unwrap_or_default();
                    let args_str = item
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .filter(|args| !args.is_empty())
                        .map(str::to_string)
                        .or_else(|| pending.as_ref().map(|call| call.partial_json.clone()))
                        .unwrap_or_else(|| "{}".to_string());
                    let arguments: Value = serde_json::from_str(&args_str).unwrap_or(json!({}));
                    let stored_id = if item_id.is_empty() || item_id == call_id {
                        call_id.clone()
                    } else {
                        format!("{call_id}|{item_id}")
                    };

                    let _ = tx.send(StreamEvent::ToolCallEnd { index });
                    content_blocks.push(ContentBlock::ToolCall {
                        id: stored_id,
                        name,
                        arguments,
                        thought_signature: None,
                    });
                } else if item_type == "reasoning" {
                    // Capture encrypted reasoning blob for round-tripping.
                    if let Some(enc) = item.get("encrypted_content").and_then(|v| v.as_str()) {
                        let summary = item
                            .get("summary")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default();
                        content_blocks.push(ContentBlock::EncryptedReasoning {
                            encrypted_content: enc.to_string(),
                            summary,
                        });
                    }
                }
            }

            "response.completed" | "response.done" | "response.incomplete" => {
                if let Some(resp) = data.get("response") {
                    if let Some(u) = resp.get("usage") {
                        apply_usage(u, &mut usage);
                    }
                    let m = resp.get("model").and_then(|v| v.as_str()).unwrap_or("");
                    if !m.is_empty() {
                        model_id = m.to_string();
                    }
                    let rid = resp.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    if !rid.is_empty() {
                        response_id = rid.to_string();
                    }
                }

                // Extract encrypted reasoning from response.output[]
                extract_reasoning_from_completed(&data, &mut content_blocks);

                let stop = if has_tool_calls {
                    StopReason::ToolUse
                } else {
                    StopReason::Stop
                };
                let _ = tx.send(StreamEvent::Done {
                    message: AssistantResponse {
                        content: std::mem::take(&mut content_blocks),
                        usage: usage.clone(),
                        stop_reason: stop,
                        model: model_id.clone(),
                        response_id: response_id.clone(),
                        rate_limit: None,
                    },
                });
                completed = true;
                break;
            }

            "response.failed" => {
                let msg = data
                    .get("response")
                    .and_then(|r| r.get("error"))
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Codex request failed");
                let _ = tx.send(StreamEvent::Error {
                    message: msg.to_string(),
                });
                // Connection is still alive even though the request failed
                completed = true;
                break;
            }

            "error" => {
                let msg = data
                    .get("message")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        data.get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|v| v.as_str())
                    })
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("Codex WS error: {data}"));
                let _ = tx.send(StreamEvent::Error { message: msg });
                // Protocol-level error; connection may still be alive
                completed = true;
                break;
            }

            _ => {} // Ignore other event types (ping, etc.)
        }
    }

    Ok(completed)
}

#[cfg(test)]
mod tests;
