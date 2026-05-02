use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{
    AssistantResponse, ChatMessage, ContentBlock, RateLimitSnapshot, Role, StopReason, StreamConfig,
    StreamEvent, ToolDef, Usage,
};

/// Streaming call to OpenRouter's OpenAI-compatible chat completions API.
pub async fn stream(
    api_key: &str,
    base_url: &str,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    _prompt_cache_key: Option<&str>,
) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
    stream_with_provider(
        "OpenRouter",
        api_key,
        base_url,
        model,
        system_prompt,
        messages,
        tools,
        _prompt_cache_key,
    )
    .await
}

/// Streaming call to a generic OpenAI-compatible chat completions API.
#[allow(clippy::too_many_arguments)]
pub async fn stream_with_provider(
    provider_name: &str,
    api_key: &str,
    base_url: &str,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    _prompt_cache_key: Option<&str>,
) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
    if let Some(partner_base) = super::vertex::openapi_base_to_anthropic_partner_base(base_url, model)
    {
        return super::anthropic::stream_vertex_anthropic_partner(
            api_key,
            &partner_base,
            model,
            system_prompt,
            messages,
            tools,
            &StreamConfig::default(),
        )
        .await;
    }

    let body = build_request_body(model, system_prompt, messages, tools);
    let url = super::openai_chat_completions_url(base_url);

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("content-type", "application/json".parse()?);
    headers.insert("authorization", format!("Bearer {api_key}").parse()?);
    if let Some(project) = super::vertex::extract_project(base_url)
        && let Ok(value) = project.parse()
    {
        headers.insert("x-goog-user-project", value);
    }

    super::log_api_request(&url, &headers, &body);

    let client = super::build_streaming_client(std::time::Duration::from_secs(300))?;

    let response = client
        .post(&url)
        .headers(headers)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("failed to connect to {provider_name} API"))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        super::log_api_error(status, &text);
        bail!("{provider_name} API error ({}): {}", status, text);
    }

    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        let rate_limit = RateLimitSnapshot::from_openai_headers(response.headers()).into_option();
        if let Err(e) = parse_sse_stream(response, rate_limit, &tx).await {
            let _ = tx.send(StreamEvent::Error {
                message: format!("{e:#}"),
            });
        }
    });

    Ok(rx)
}

// ---------------------------------------------------------------------------
// Request body (OpenAI chat completions format)
// ---------------------------------------------------------------------------

fn build_request_body(
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
) -> Value {
    openai_compat_chat_completion_body(model, system_prompt, messages, tools)
}

pub(super) fn openai_compat_chat_completion_body(
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
) -> Value {
    let mut api_messages: Vec<Value> = Vec::new();

    // System prompt
    if !system_prompt.is_empty() {
        api_messages.push(json!({
            "role": "system",
            "content": system_prompt,
        }));
    }

    for msg in messages {
        match msg.role {
            Role::User => {
                let mut pending_user_parts: Vec<Value> = Vec::new();

                fn flush_openrouter_user(api_messages: &mut Vec<Value>, parts: &mut Vec<Value>) {
                    if parts.is_empty() {
                        return;
                    }
                    let multimodal = parts.len() > 1
                        || parts
                            .iter()
                            .any(|p| p.get("type").and_then(|v| v.as_str()) == Some("image_url"));
                    if multimodal {
                        api_messages.push(json!({
                            "role": "user",
                            "content": parts.clone(),
                        }));
                    } else if let Some(t) = parts[0].get("text").and_then(|v| v.as_str()) {
                        api_messages.push(json!({
                            "role": "user",
                            "content": t,
                        }));
                    }
                    parts.clear();
                }

                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => {
                            if !text.is_empty() {
                                pending_user_parts.push(json!({
                                    "type": "text",
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
                            pending_user_parts.push(json!({
                                "type": "image_url",
                                "image_url": { "url": url },
                            }));
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            flush_openrouter_user(&mut api_messages, &mut pending_user_parts);
                            api_messages.push(json!({
                                "role": "tool",
                                "tool_call_id": super::sanitize_id_openai(tool_use_id),
                                "content": content,
                            }));
                        }
                        _ => {}
                    }
                }
                flush_openrouter_user(&mut api_messages, &mut pending_user_parts);
            }
            Role::Assistant => {
                let text: String = super::openai_compat_assistant_join_text(&msg.content);

                let tool_calls: Vec<Value> = msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                            ..
                        } => Some(json!({
                            "id": super::sanitize_id_openai(id),
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": arguments.to_string(),
                            }
                        })),
                        _ => None,
                    })
                    .collect();

                // Reasoning replay: DeepSeek thinking mode rejects follow-up turns when
                // `reasoning_content` is omitted or empty after tool_calls — pydantic-ai
                // surfaced this as `"Missing reasoning_content field"`. Gateways sometimes
                // fold CoT into ordinary `delta.content`; mirror that server-side split by
                // replaying plain text that appeared before the first tool_use.
                let is_deepseek = model.to_ascii_lowercase().contains("deepseek");
                let mut reasoning_wire: String =
                    super::openai_compat_assistant_concat_reasoning_chunks(&msg.content);
                if reasoning_wire.is_empty() && !tool_calls.is_empty() {
                    reasoning_wire = super::openai_plain_text_before_first_tool_call(&msg.content);
                }

                let mut msg_obj = json!({"role": "assistant"});
                if !text.is_empty() {
                    msg_obj["content"] = json!(text);
                } else if tool_calls.is_empty() {
                    // Must have content or tool_calls
                    msg_obj["content"] = json!("");
                }
                if !tool_calls.is_empty() {
                    msg_obj["tool_calls"] = json!(tool_calls);
                }

                let need_reasoning_keys =
                    !reasoning_wire.is_empty() || (is_deepseek && !tool_calls.is_empty());
                if need_reasoning_keys {
                    msg_obj["reasoning_content"] = json!(reasoning_wire);
                    msg_obj["reasoning"] = json!(reasoning_wire);
                }
                api_messages.push(msg_obj);
            }
        }
    }

    let api_tools: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                }
            })
        })
        .collect();

    let mut body = json!({
        "model": model,
        "messages": api_messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });

    if !api_tools.is_empty() {
        body["tools"] = json!(api_tools);
        body["tool_choice"] = json!("auto");
    }

    maybe_add_deepseek_compat_thinking(model, &mut body);
    maybe_add_anthropic_cache_control(model, &mut body);

    body
}

/// DeepSeek "thinking mode" (see api-docs.deepseek.com `thinking_mode`): when enabled,
/// **`reasoning_content` must be replayed after tool loops**. OpenCode's gateway may
/// already enable thinking, but emitting these fields matches their official OpenAI-compat
/// example and avoids subtle server-side inconsistencies.
fn maybe_add_deepseek_compat_thinking(model: &str, body: &mut Value) {
    if !model.to_ascii_lowercase().contains("deepseek") {
        return;
    }
    body["thinking"] = json!({ "type": "enabled" });
    body["reasoning_effort"] = json!("high");
}

/// Pull text-ish reasoning fragments from heterogeneous OpenAI-compat / gateway JSON.
fn append_openai_compat_reasoning_json(buf: &mut String, v: &Value) {
    if let Some(s) = v.as_str() {
        if !s.is_empty() {
            buf.push_str(s);
        }
        return;
    }
    if let Some(map) = v.as_object() {
        if let Some(s) = map
            .get("text")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            buf.push_str(s);
        } else if let Some(inner) = map.get("reasoning") {
            append_openai_compat_reasoning_json(buf, inner);
        }
        return;
    }
    if let Some(arr) = v.as_array() {
        for item in arr {
            append_openai_compat_reasoning_json(buf, item);
        }
    }
}

fn ingest_openai_sse_reasoning_from_delta(buf: &mut String, delta: &Value) {
    for key in ["reasoning_content", "reasoning", "thinking"] {
        if let Some(v) = delta.get(key) {
            append_openai_compat_reasoning_json(buf, v);
        }
    }
}

fn ingest_openai_sse_reasoning_from_message(buf: &mut String, msg: &Value) {
    for key in ["reasoning_content", "reasoning", "thinking"] {
        if let Some(v) = msg.get(key) {
            append_openai_compat_reasoning_json(buf, v);
        }
    }
}

// ---------------------------------------------------------------------------
// SSE stream parsing (OpenAI chat.completion.chunk format)
// ---------------------------------------------------------------------------

struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
    index: usize,
}

struct OpenAiCompletionAccum {
    text_buf: String,
    reasoning_buf: String,
    usage: Usage,
    model_id: String,
    finish_reason: Option<String>,
    pending_tool_calls: Vec<PendingToolCall>,
}

impl Default for OpenAiCompletionAccum {
    fn default() -> Self {
        Self {
            text_buf: String::new(),
            reasoning_buf: String::new(),
            usage: Usage::default(),
            model_id: String::new(),
            finish_reason: None,
            pending_tool_calls: Vec::new(),
        }
    }
}

fn ingest_openai_completion_chunk_payload(
    data: &Value,
    accum: &mut OpenAiCompletionAccum,
    tx: &mpsc::UnboundedSender<StreamEvent>,
) -> Result<()> {
    if let Some(msg) = openai_compat_stream_error_message(data) {
        bail!("{msg}");
    }
    if accum.model_id.is_empty()
        && let Some(m) = data.get("model").and_then(|v| v.as_str())
    {
        accum.model_id = m.to_string();
    }
    if let Some(u) = data.get("usage") {
        apply_usage(u, &mut accum.usage);
    }
    let choices = match data.get("choices").and_then(|v| v.as_array()) {
        Some(c) => c,
        None => return Ok(()),
    };

    for choice in choices {
        if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            accum.finish_reason = Some(fr.to_string());
        }

        let delta = match choice.get("delta") {
            Some(d) => d,
            None => continue,
        };

        if let Some(content) = delta.get("content").and_then(|v| v.as_str())
            && !content.is_empty()
        {
            accum.text_buf.push_str(content);
            let _ = tx.send(StreamEvent::TextDelta {
                delta: content.to_string(),
            });
        }

        ingest_openai_sse_reasoning_from_delta(&mut accum.reasoning_buf, delta);
        if let Some(msg) = choice.get("message") {
            let mut snap = String::new();
            ingest_openai_sse_reasoning_from_message(&mut snap, msg);
            if snap.len() > accum.reasoning_buf.len() {
                accum.reasoning_buf.clear();
                accum.reasoning_buf.push_str(&snap);
            }
        }

        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tool_calls {
                let tc_index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                    let name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let initial_args = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    while accum.pending_tool_calls.len() <= tc_index {
                        accum.pending_tool_calls.push(PendingToolCall {
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                            index: accum.pending_tool_calls.len(),
                        });
                    }
                    let prior_args =
                        std::mem::take(&mut accum.pending_tool_calls[tc_index].arguments);
                    let merged_args = format!("{prior_args}{initial_args}");
                    accum.pending_tool_calls[tc_index] = PendingToolCall {
                        id: id.to_string(),
                        name: name.clone(),
                        arguments: merged_args,
                        index: tc_index,
                    };

                    let _ = tx.send(StreamEvent::ToolCallStart {
                        index: tc_index,
                        id: id.to_string(),
                        name,
                    });
                    if !initial_args.is_empty() {
                        let _ = tx.send(StreamEvent::ToolCallDelta {
                            index: tc_index,
                            delta: initial_args,
                        });
                    }
                } else if let Some(args_delta) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|v| v.as_str())
                {
                    while accum.pending_tool_calls.len() <= tc_index {
                        accum.pending_tool_calls.push(PendingToolCall {
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                            index: accum.pending_tool_calls.len(),
                        });
                    }
                    accum.pending_tool_calls[tc_index].index = tc_index;
                    accum.pending_tool_calls[tc_index]
                        .arguments
                        .push_str(args_delta);
                    let _ = tx.send(StreamEvent::ToolCallDelta {
                        index: tc_index,
                        delta: args_delta.to_string(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn finalize_openai_completion_accum(
    accum: OpenAiCompletionAccum,
    rate_limit: Option<RateLimitSnapshot>,
    tx: &mpsc::UnboundedSender<StreamEvent>,
) {
    let mut content_blocks: Vec<ContentBlock> = Vec::new();
    if !accum.reasoning_buf.is_empty() {
        content_blocks.push(ContentBlock::Reasoning {
            text: accum.reasoning_buf,
        });
    }
    if !accum.text_buf.is_empty() {
        content_blocks.push(ContentBlock::Text {
            text: accum.text_buf,
        });
    }

    for tc in &accum.pending_tool_calls {
        if tc.id.is_empty() {
            continue;
        }
        let _ = tx.send(StreamEvent::ToolCallEnd { index: tc.index });
        let arguments: Value = serde_json::from_str(&tc.arguments).unwrap_or(json!({}));
        content_blocks.push(ContentBlock::ToolCall {
            id: tc.id.clone(),
            name: tc.name.clone(),
            arguments,
            thought_signature: None,
        });
    }

    let stop = match accum.finish_reason.as_deref() {
        Some("tool_calls") => StopReason::ToolUse,
        Some("length") => StopReason::Length,
        _ => {
            if accum.pending_tool_calls.iter().any(|tc| !tc.id.is_empty()) {
                StopReason::ToolUse
            } else {
                StopReason::Stop
            }
        }
    };

    let _ = tx.send(StreamEvent::Done {
        message: AssistantResponse {
            content: content_blocks,
            usage: accum.usage,
            stop_reason: stop,
            model: accum.model_id,
            response_id: String::new(),
            rate_limit,
        },
    });
}

pub(super) async fn parse_openai_completion_chunk_byte_stream<S>(
    stream: S,
    rate_limit: Option<RateLimitSnapshot>,
    tx: &mpsc::UnboundedSender<StreamEvent>,
) -> Result<()>
where
    S: futures_util::Stream<Item = std::result::Result<bytes::Bytes, anyhow::Error>> + Send,
{
    use futures_util::{StreamExt, pin_mut};

    pin_mut!(stream);
    let mut accum = OpenAiCompletionAccum::default();
    let mut total_bytes = 0usize;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error reading Bedrock OpenAI-compat chunk stream")?;
        total_bytes += chunk.len();
        let data: Value = serde_json::from_slice(chunk.as_ref()).with_context(|| {
            format!(
                "invalid Bedrock OpenAI chunk JSON after {} bytes: {}",
                total_bytes,
                String::from_utf8_lossy(chunk.as_ref())
            )
        })?;
        ingest_openai_completion_chunk_payload(&data, &mut accum, tx)?;
    }
    finalize_openai_completion_accum(accum, rate_limit, tx);
    Ok(())
}

async fn parse_sse_stream(
    response: reqwest::Response,
    rate_limit: Option<RateLimitSnapshot>,
    tx: &mpsc::UnboundedSender<StreamEvent>,
) -> Result<()> {
    let mut stream = response.bytes_stream();
    let mut decoder = super::SseDecoder::new();
    let mut accum = OpenAiCompletionAccum::default();

    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error reading SSE chunk")?;
        decoder.push_chunk(&chunk);

        while let Some(event) = decoder.next_event() {
            let data: Value = match super::parse_sse_json(&event) {
                Some(v) => v,
                None => continue,
            };
            ingest_openai_completion_chunk_payload(&data, &mut accum, tx)?;
        }
    }

    finalize_openai_completion_accum(accum, rate_limit, tx);
    Ok(())
}

/// Best-effort error text from OpenAI-style or OpenCode-shaped stream payloads.
pub(super) fn openai_compat_stream_error_message(v: &Value) -> Option<String> {
    // OpenCode: `{"type":"error","error":{"type":"AuthError","message":"..."}}`
    if v.get("type").and_then(|t| t.as_str()) == Some("error") {
        return v
            .pointer("/error/message")
            .and_then(|x| x.as_str())
            .map(String::from)
            .or_else(|| v.get("error").map(std::string::ToString::to_string));
    }
    // OpenAI: `{"error":{"message":"...","type":"..."}}`
    match v.get("error") {
        None | Some(Value::Null) => None,
        Some(err) => Some(openai_compat_error_detail(err)),
    }
}

fn openai_compat_error_detail(err: &Value) -> String {
    let msg = err
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let typ = err.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if msg.is_empty() {
        err.to_string()
    } else if typ.is_empty() {
        msg
    } else {
        format!("{typ}: {msg}")
    }
}

#[cfg(test)]
mod stream_error_tests {
    use serde_json::json;

    use super::{openai_compat_error_detail, openai_compat_stream_error_message};

    #[test]
    fn stream_error_detects_opencode_envelope() {
        let v = json!({"type":"error","error":{"type":"AuthError","message":"Invalid API key."}});
        assert_eq!(
            openai_compat_stream_error_message(&v).as_deref(),
            Some("Invalid API key.")
        );
    }

    #[test]
    fn stream_error_detects_openai_error_object() {
        let v =
            json!({"error":{"type":"invalid_request_error","message":"max_tokens is too large"}});
        assert_eq!(
            openai_compat_stream_error_message(&v).as_deref(),
            Some("invalid_request_error: max_tokens is too large")
        );
        assert!(
            openai_compat_stream_error_message(&json!({
                "object": "chat.completion.chunk",
                "choices": [],
                "usage": {"prompt_tokens": 1}
            }))
            .is_none()
        );
    }

    #[test]
    fn openai_error_detail_fallback() {
        let err = json!({});
        assert_eq!(openai_compat_error_detail(&err), "{}");
    }
}

fn supports_anthropic_cache_control(model: &str) -> bool {
    model.to_ascii_lowercase().contains("claude")
}

fn maybe_add_anthropic_cache_control(model: &str, body: &mut Value) {
    if !supports_anthropic_cache_control(model) {
        return;
    }

    let Some(messages) = body.get_mut("messages").and_then(|v| v.as_array_mut()) else {
        return;
    };

    for msg in messages.iter_mut().rev() {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role != "user" && role != "assistant" {
            continue;
        }

        let Some(content) = msg.get_mut("content") else {
            continue;
        };

        if let Some(text) = content.as_str() {
            *content = json!([{
                "type": "text",
                "text": text,
                "cache_control": {"type": "ephemeral"},
            }]);
            return;
        }

        let Some(parts) = content.as_array_mut() else {
            continue;
        };

        for part in parts.iter_mut().rev() {
            if part.get("type").and_then(|v| v.as_str()) == Some("text") {
                part["cache_control"] = json!({"type": "ephemeral"});
                return;
            }
        }
    }
}

pub(super) fn apply_usage(u: &Value, usage: &mut Usage) {
    let prompt_total = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    usage.output_tokens = u
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    let details = u.get("prompt_tokens_details");
    usage.cache_read_tokens = details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    usage.cache_write_tokens = details
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    usage.input_tokens = prompt_total
        .saturating_sub(usage.cache_read_tokens)
        .saturating_sub(usage.cache_write_tokens);
}

#[cfg(test)]
mod tests;
