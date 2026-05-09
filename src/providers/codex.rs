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

/// Codex Responses API transport — affects request body shape.
///
/// OpenAI WebSocket mode ([guide](https://developers.openai.com/api/docs/guides/websocket-mode)):
/// do not send transport-specific fields such as `stream` or `background`.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CodexTransport {
    HttpPost,
    WebSocket,
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
        CodexTransport::HttpPost,
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
    _previous_response_id: Option<&str>,
    config: &super::StreamConfig,
    transport: CodexTransport,
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
        "store": false,
        "include": ["reasoning.encrypted_content"],
    });

    // SSE (`POST …/codex/responses`): streaming responses use `stream: true`.
    // WebSocket mode omits `stream` — see OpenAI WebSocket mode guide.
    if matches!(transport, CodexTransport::HttpPost) {
        body["stream"] = json!(true);
    }

    // Stateful chaining (`previous_response_id` + incremental `input`) is documented for
    // WebSocket performance — see [WebSocket mode](https://developers.openai.com/api/docs/guides/websocket-mode).
    // Sidecar still sends full prepared history each turn; enabling `previous_response_id` safely
    // requires emitting only **new** input items on continuation turns (not implemented yet).
    // `previous_response_id` remains plumbed through for that follow-up.

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
                        let partial_json_raw =
                            codex_arguments_field_to_string(item.get("arguments"));
                        let partial_json = if is_placeholder_arguments_json(&partial_json_raw) {
                            String::new()
                        } else {
                            partial_json_raw
                        };
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
                                done_json: None,
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
                        let from_done = codex_arguments_field_to_string(data.get("arguments"));
                        if !from_done.is_empty() && !is_placeholder_arguments_json(&from_done) {
                            call.done_json = Some(from_done);
                        }
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
                        let arguments = resolve_codex_tool_arguments(item, pending.as_ref());
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
                            rate_limit: merged_codex_rate_limit_for_done(rate_limit.clone(), &data),
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
    done_json: Option<String>,
    name: String,
}

/// Normalize `function_call.arguments` from the Responses API into a string we
/// merge with `function_call_arguments.delta` chunks. The field can be a JSON
/// string, object, array, or absent.
fn codex_arguments_field_to_string(args: Option<&Value>) -> String {
    match args {
        None => String::new(),
        Some(Value::String(s)) => s.to_string(),
        Some(v) => serde_json::to_string(v).unwrap_or_default(),
    }
}

fn is_placeholder_arguments_json(s: &str) -> bool {
    matches!(s.trim(), "" | "{}")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum ArgumentsCandidateSource {
    InlineObject,
    InlineString,
    DoneEvent,
    Streamed,
}

#[derive(Debug)]
struct ParsedArgumentsCandidate {
    source: ArgumentsCandidateSource,
    raw_len: usize,
    value: Value,
}

fn push_parsed_arguments_candidate(
    candidates: &mut Vec<ParsedArgumentsCandidate>,
    source: ArgumentsCandidateSource,
    raw: &str,
) {
    let trimmed = raw.trim();
    if is_placeholder_arguments_json(trimmed) {
        return;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        candidates.push(ParsedArgumentsCandidate {
            source,
            raw_len: trimmed.len(),
            value,
        });
    }
}

fn parsed_arguments_candidate_score(candidate: &ParsedArgumentsCandidate) -> (usize, usize, usize) {
    let shape_score = match &candidate.value {
        Value::Object(map) => 3 + map.len(),
        Value::Array(items) => 2 + items.len(),
        Value::Null => 0,
        _ => 1,
    };
    let source_score = match candidate.source {
        ArgumentsCandidateSource::InlineObject => 1,
        ArgumentsCandidateSource::InlineString => 2,
        ArgumentsCandidateSource::DoneEvent => 3,
        ArgumentsCandidateSource::Streamed => 4,
    };
    (shape_score, candidate.raw_len, source_score)
}

/// Build the final tool `arguments` value for execution. Prefer a non-empty
/// valid argument payload from inline fields, `function_call_arguments.done`, or
/// streamed `function_call_arguments.delta`.
///
/// The API sometimes finishes with `arguments: "{}"` or an empty object while
/// the real payload only arrived via `response.function_call_arguments.delta`
/// (common for large / multiline JSON). It can also emit a non-empty but
/// truncated inline string for large payloads. We therefore parse every
/// candidate we observed and keep the most complete valid JSON value.
fn resolve_codex_tool_arguments(item: &Value, pending: Option<&PendingToolCall>) -> Value {
    let mut candidates = Vec::new();

    if let Some(Value::Object(map)) = item.get("arguments")
        && !map.is_empty()
    {
        candidates.push(ParsedArgumentsCandidate {
            source: ArgumentsCandidateSource::InlineObject,
            raw_len: serde_json::to_string(map).map(|s| s.len()).unwrap_or(0),
            value: Value::Object(map.clone()),
        });
    } else {
        push_parsed_arguments_candidate(
            &mut candidates,
            ArgumentsCandidateSource::InlineString,
            &codex_arguments_field_to_string(item.get("arguments")),
        );
    }

    if let Some(pending) = pending {
        if let Some(done_json) = pending.done_json.as_deref() {
            push_parsed_arguments_candidate(
                &mut candidates,
                ArgumentsCandidateSource::DoneEvent,
                done_json,
            );
        }
        push_parsed_arguments_candidate(
            &mut candidates,
            ArgumentsCandidateSource::Streamed,
            &pending.partial_json,
        );
    }

    if let Some(best) = candidates
        .into_iter()
        .max_by_key(parsed_arguments_candidate_score)
    {
        return best.value;
    }

    let from_item = codex_arguments_field_to_string(item.get("arguments"));
    let from_done = pending
        .and_then(|call| call.done_json.as_deref())
        .unwrap_or_default();
    let from_pending = pending.map(|call| call.partial_json.as_str()).unwrap_or("");
    let prefix: String = [from_item.as_str(), from_done, from_pending]
        .iter()
        .find(|candidate| !candidate.trim().is_empty())
        .copied()
        .unwrap_or_default()
        .chars()
        .take(500)
        .collect();
    crate::broker::try_log_error(
        "codex-transport",
        "function_call arguments JSON parse failed",
        Some(&format!("args_prefix={prefix}")),
    );
    json!({})
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
// Codex quota JSON → RateLimitSnapshot (stream + `wham/usage`)
// ---------------------------------------------------------------------------

/// Map Codex `rate_limits` JSON (Responses `response.completed`, or `wham/usage` root object)
/// into [`RateLimitSnapshot`]. `% left` becomes utilization % for the same footer labels as Claude.
pub fn rate_limit_snapshot_from_codex_quota_json(data: &Value) -> Option<RateLimitSnapshot> {
    let root = data.as_object()?;
    let bucket = codex_quota_limits_bucket(data);
    let five = find_codex_limit_window(
        bucket,
        root,
        &[
            "five_hour",
            "five_hour_limit",
            "five_hour_rate_limit",
            "primary",
            "primary_window",
        ],
    );
    let weekly = find_codex_limit_window(
        bucket,
        root,
        &[
            "weekly",
            "weekly_limit",
            "weekly_rate_limit",
            "secondary",
            "secondary_window",
        ],
    );
    let mut snap = RateLimitSnapshot::default();
    if let Some(w) = five {
        if let Some(p_left) = codex_percent_left(w) {
            let util_pct = (100.0_f64 - p_left).round().clamp(0.0, 100.0) as u32;
            snap.util_5h_pct = Some(util_pct);
        }
        if let Some(e) = codex_reset_epoch_secs(w) {
            snap.reset_5h_at = Some(e);
        }
    }
    if let Some(w) = weekly {
        if let Some(p_left) = codex_percent_left(w) {
            let util_pct = (100.0_f64 - p_left).round().clamp(0.0, 100.0) as u32;
            snap.util_7d_pct = Some(util_pct);
        }
        if let Some(e) = codex_reset_epoch_secs(w) {
            snap.reset_7d_at = Some(e);
        }
    }
    snap.into_option()
}

pub(crate) fn rate_limit_snapshot_from_codex_completed_event(
    data: &Value,
) -> Option<RateLimitSnapshot> {
    if let Some(r) = data.get("response") {
        if let Some(s) = rate_limit_snapshot_from_codex_quota_json(r) {
            return Some(s);
        }
    }
    rate_limit_snapshot_from_codex_quota_json(data)
}

fn merged_codex_rate_limit_for_done(
    header_or_ws_snap: Option<RateLimitSnapshot>,
    event: &Value,
) -> Option<RateLimitSnapshot> {
    let stream = rate_limit_snapshot_from_codex_completed_event(event);
    RateLimitSnapshot::overlay_option(header_or_ws_snap, stream)
}

// ---------------------------------------------------------------------------
// ChatGPT Codex plan quota (`/backend-api/wham/usage`)
// ---------------------------------------------------------------------------

/// Raw JSON from `wham/usage` (same shape as stream `rate_limits` in many builds).
pub async fn fetch_codex_plan_quota_json(
    access_token: &str,
    account_id: &str,
) -> Result<Value, String> {
    if access_token.is_empty() {
        return Err("missing Codex access token".into());
    }
    if account_id.is_empty() {
        return Err("missing ChatGPT account id".into());
    }
    let url = "https://chatgpt.com/backend-api/wham/usage";
    let client = super::build_streaming_client(std::time::Duration::from_secs(20))
        .map_err(|e| e.to_string())?;
    let response = client
        .get(url)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Accept", "application/json")
        .header("ChatGPT-Account-Id", account_id)
        .header("Origin", "https://chatgpt.com")
        .header("Referer", "https://chatgpt.com/")
        .header("originator", "sidekar")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let snippet: String = text.chars().take(240).collect();
        return Err(format!("HTTP {status}: {snippet}"));
    }
    response.json().await.map_err(|e| e.to_string())
}

/// Poll Codex ChatGPT plan quota (rolling ~5h + weekly windows). Same bearer +
/// account id as Codex Responses API.
///
/// Endpoint is not part of OpenAI's published Responses reference; Community +
/// Codex CLI tooling rely on it for quota summaries.
pub async fn fetch_codex_plan_quota_body(
    access_token: &str,
    account_id: &str,
) -> Result<String, String> {
    let v = fetch_codex_plan_quota_json(access_token, account_id).await?;
    format_codex_wham_usage_body(&v).ok_or_else(|| {
        let snippet = serde_json::to_string(&v).unwrap_or_default();
        let snippet: String = snippet.chars().take(280).collect();
        format!("unexpected wham/usage JSON shape: {snippet}")
    })
}

fn codex_quota_limits_bucket(data: &Value) -> Option<&serde_json::Map<String, Value>> {
    data.get("rate_limit")
        .and_then(|v| v.as_object())
        .or_else(|| data.get("rate_limits").and_then(|v| v.as_object()))
}

fn resolve_codex_window_blob(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    let obj = value.as_object()?;
    if obj.contains_key("percent_left")
        || obj.contains_key("remaining_percent")
        || obj.contains_key("used_percent")
        || obj.contains_key("reset_at")
        || obj.contains_key("reset_time_ms")
    {
        return Some(obj);
    }
    obj.get("primary_window").and_then(|v| v.as_object())
}

fn find_codex_limit_window<'a>(
    bucket: Option<&'a serde_json::Map<String, Value>>,
    root: &'a serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<&'a serde_json::Map<String, Value>> {
    let mut maps: Vec<&serde_json::Map<String, Value>> = Vec::new();
    if let Some(b) = bucket {
        maps.push(b);
    }
    maps.push(root);
    for map in maps {
        for k in keys {
            if let Some(v) = map.get(*k) {
                if let Some(inner) = resolve_codex_window_blob(v) {
                    return Some(inner);
                }
            }
        }
    }
    None
}

fn codex_percent_left(window: &serde_json::Map<String, Value>) -> Option<f64> {
    if let Some(p) = window
        .get("percent_left")
        .and_then(|v| v.as_f64())
        .or_else(|| window.get("remaining_percent").and_then(|v| v.as_f64()))
    {
        return Some(p);
    }
    window
        .get("used_percent")
        .and_then(|v| v.as_f64())
        .map(|u| (100.0 - u).max(0.0))
}

fn codex_reset_epoch_secs(window: &serde_json::Map<String, Value>) -> Option<u64> {
    let raw = window
        .get("reset_time_ms")
        .or_else(|| window.get("reset_at"))
        .and_then(|v| v.as_u64())?;
    Some(if raw > 100_000_000_000 {
        raw / 1000
    } else {
        raw
    })
}

fn format_codex_quota_reset_countdown(epoch_secs: u64) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if epoch_secs <= now {
        return "now".into();
    }
    let delta = epoch_secs - now;
    if delta < 60 {
        format!("in {delta}s")
    } else if delta < 3600 {
        format!("in {}m", delta / 60)
    } else if delta < 86400 {
        format!("in {}h{}m", delta / 3600, (delta % 3600) / 60)
    } else {
        format!("in {}d{}h", delta / 86400, (delta % 86400) / 3600)
    }
}

fn format_codex_limit_line(label: &str, window: &serde_json::Map<String, Value>) -> String {
    let pct = codex_percent_left(window)
        .map(|p| format!("{p:.1}% left"))
        .unwrap_or_else(|| "—".into());
    let tail = codex_reset_epoch_secs(window)
        .map(|e| format!(" · resets {}", format_codex_quota_reset_countdown(e)))
        .unwrap_or_default();
    format!("  {:<18}{}{}\n", label, pct, tail)
}

fn format_codex_wham_usage_body(data: &Value) -> Option<String> {
    let root = data.as_object()?;
    let bucket = codex_quota_limits_bucket(data);
    let five = find_codex_limit_window(
        bucket,
        root,
        &[
            "five_hour",
            "five_hour_limit",
            "five_hour_rate_limit",
            "primary",
            "primary_window",
        ],
    );
    let weekly = find_codex_limit_window(
        bucket,
        root,
        &[
            "weekly",
            "weekly_limit",
            "weekly_rate_limit",
            "secondary",
            "secondary_window",
        ],
    );
    let mut out = String::new();
    if let Some(w) = five {
        out.push_str(&format_codex_limit_line("5h window", w));
    }
    if let Some(w) = weekly {
        out.push_str(&format_codex_limit_line("weekly window", w));
    }
    if out.is_empty() { None } else { Some(out) }
}

// ---------------------------------------------------------------------------
// WebSocket transport
// ---------------------------------------------------------------------------

/// Rate-limit snapshot from WS HTTP handshake (`x-ratelimit-*`), mirroring SSE POST path.
fn ws_handshake_rate_limit<B>(resp: &http::Response<B>) -> Option<RateLimitSnapshot> {
    RateLimitSnapshot::from_openai_headers(resp.headers()).into_option()
}

/// Open a fresh WebSocket connection to the Codex Responses API.
///
/// Handles both direct and MITM-proxy paths. Returns split write+read halves plus
/// handshake headers-derived limits when present.
async fn connect_ws(
    api_key: &str,
    account_id: &str,
    base_url: &str,
    verbose: bool,
) -> Result<(WsWrite, WsRead, Option<RateLimitSnapshot>)> {
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

    let (ws, handshake_rl) = if let Some((proxy_port, _)) = super::attached_mitm_for_custom_tls() {
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
        let (ws, resp) =
            tokio_tungstenite::client_async_tls_with_config(ws_request, tcp, None, connector)
                .await
                .context("WS handshake over proxy tunnel failed")?;
        (ws, ws_handshake_rate_limit(&resp))
    } else {
        if verbose {
            log_ws_verbose(
                "tls-ws-handshake",
                Some(&format!("host={host} via_proxy=false")),
            );
        }
        let connector = tokio_tungstenite::Connector::Rustls(tls_config);
        let (ws, resp) = tokio_tungstenite::connect_async_tls_with_config(
            ws_request,
            None,
            false,
            Some(connector),
        )
        .await
        .context("failed to connect WebSocket to Codex API")?;
        (ws, ws_handshake_rate_limit(&resp))
    };
    if verbose {
        log_ws_verbose("connected", None);
    }

    let (w, r) = futures_util::StreamExt::split(ws);
    Ok((w, r, handshake_rl))
}

/// Stream a codex response over WebSocket instead of SSE.
///
/// Same payload fields as the HTTP POST path except:
/// - Protocol: `wss://…/codex/responses` instead of `https://`
/// - Header: `OpenAI-Beta: responses_websockets=2026-02-06`
/// - No `stream` key in JSON ([WebSocket mode guide](https://developers.openai.com/api/docs/guides/websocket-mode))
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
        CodexTransport::WebSocket,
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
    let (write, mut read, first_text, handshake_rl) = 'conn: {
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
                    break 'conn (w, r, Some(t.to_string()), None);
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
        let (mut w, r, rl) = connect_ws(api_key, account_id, base_url, verbose).await?;
        if verbose {
            log_ws_verbose("sending-response-create", None);
        }
        w.send(WsMessage::Text(payload.into()))
            .await
            .context("failed to send response.create over WS")?;
        (w, r, None, rl)
    };

    let (tx, rx) = mpsc::unbounded_channel();
    let (reclaim_tx, reclaim_rx) = tokio::sync::oneshot::channel();

    let verbose = super::is_verbose();
    tokio::spawn(async move {
        match parse_ws_stream(&mut read, &tx, first_text, handshake_rl).await {
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
///
/// `rate_limit` comes from WS HTTP handshake headers on fresh connections only;
/// reused sockets skip handshake this turn so it is typically `None`.
async fn parse_ws_stream<S>(
    read: &mut futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
    tx: &mpsc::UnboundedSender<StreamEvent>,
    first_text: Option<String>,
    rate_limit: Option<RateLimitSnapshot>,
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
                    let partial_json_raw = codex_arguments_field_to_string(item.get("arguments"));
                    let partial_json = if is_placeholder_arguments_json(&partial_json_raw) {
                        String::new()
                    } else {
                        partial_json_raw
                    };
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
                            done_json: None,
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
                    let from_done = codex_arguments_field_to_string(data.get("arguments"));
                    if !from_done.is_empty() && !is_placeholder_arguments_json(&from_done) {
                        call.done_json = Some(from_done);
                    }
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
                    let arguments = resolve_codex_tool_arguments(item, pending.as_ref());
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
                        rate_limit: merged_codex_rate_limit_for_done(rate_limit.clone(), &data),
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
                let nested = data.get("error");
                let code = nested
                    .and_then(|e| e.get("code"))
                    .and_then(|v| v.as_str())
                    .or_else(|| data.get("code").and_then(|v| v.as_str()));
                let mut msg = data
                    .get("message")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        nested
                            .and_then(|e| e.get("message"))
                            .and_then(|v| v.as_str())
                    })
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("Codex WS error: {data}"));
                if let Some(c) = code {
                    msg = format!("{msg} ({c})");
                }
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
