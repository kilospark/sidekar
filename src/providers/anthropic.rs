use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use xxhash_rust::xxh64::xxh64;

use super::{
    AssistantResponse, ChatMessage, ContentBlock, RateLimitSnapshot, Role, StopReason,
    StreamConfig, StreamEvent, ToolDef, Usage,
};

const CLAUDE_CODE_VERSION: &str = "2.1.87";
const FINGERPRINT_SALT: &str = "59cf53e54c78";
const CCH_PLACEHOLDER: &str = "cch=00000";
const CCH_SEED: u64 = 0x6E52_736A_C806_831E;
const CCH_MASK: u64 = 0xF_FFFF;

/// Stream a response from the Anthropic Messages API.
#[allow(clippy::too_many_arguments)]
pub async fn stream(
    api_key: &str,
    base_url: &str,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    _prompt_cache_key: Option<&str>,
    config: &StreamConfig,
) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
    let is_oauth = api_key.contains("sk-ant-oat");

    // The REPL surfaces a `<id>#1m` variant for models that support 1M
    // context. For beta-gated models (Sonnet 4/4.5, Opus 4.5), this enables
    // the `context-1m-2025-08-07` header. For native-1M models (Opus 4.7/4.6,
    // Sonnet 4.6), the header is harmless/no-op. Strip the suffix before
    // talking to Anthropic.
    let (model, enable_1m_beta) = match model.strip_suffix(super::ANTHROPIC_1M_SUFFIX) {
        Some(clean) => (clean, true),
        None => (model, false),
    };

    let body = build_request_body(
        api_key,
        model,
        system_prompt,
        messages,
        tools,
        config,
        is_oauth,
    );
    let mut body_json = serde_json::to_string(&body)?;
    if is_oauth && body_json.contains(CCH_PLACEHOLDER) {
        body_json = sign_request_body(&body_json);
    }
    // Claude Code's captured traffic hits `/v1/messages?beta=true`, not bare
    // `/v1/messages`. The `?beta=true` query flag is what actually activates
    // the beta features listed in `anthropic-beta` — including the
    // `prompt-caching-scope-2026-01-05` beta. Without it, scope is accepted
    // syntactically but the cache never creates (cache_creation stays 0).
    let url = format!(
        "{}/v1/messages{}",
        base_url.trim_end_matches('/'),
        if is_oauth { "?beta=true" } else { "" }
    );

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("content-type", "application/json".parse()?);
    headers.insert("accept", "application/json".parse()?);
    headers.insert("anthropic-version", "2023-06-01".parse()?);
    headers.insert("anthropic-dangerous-direct-browser-access", "true".parse()?);

    // Base beta list per auth mode. `context-1m-2025-08-07` is appended below
    // when the user picked the `#1m` variant of an eligible model.
    let mut beta_list = if is_oauth {
        // `prompt-caching-scope-2026-01-05` gates the `cache_control.ephemeral.
        // scope` field. Without it, Anthropic returns 400
        // `system.N.cache_control.ephemeral.scope: Extra inputs are not
        // permitted`. Claude Code sends this beta in its captured traffic,
        // and it's what lets the cache survive the volatile per-request
        // billing header (block 0, which changes every turn because `cch`
        // is a body hash).
        String::from(
            "claude-code-20250219,oauth-2025-04-20,fine-grained-tool-streaming-2025-05-14,prompt-caching-scope-2026-01-05",
        )
    } else {
        String::from("fine-grained-tool-streaming-2025-05-14")
    };
    if enable_1m_beta {
        beta_list.push_str(",context-1m-2025-08-07");
    }
    headers.insert("anthropic-beta", beta_list.parse()?);

    if is_oauth {
        headers.insert("authorization", format!("Bearer {api_key}").parse()?);
        headers.insert("user-agent", "claude-cli/2.1.87".parse()?);
        headers.insert("x-app", "cli".parse()?);
    } else {
        headers.insert("x-api-key", api_key.parse()?);
    }

    if let Ok(log_body) = serde_json::from_str::<Value>(&body_json) {
        super::log_api_request(&url, &headers, &log_body);
    }

    let client = super::build_streaming_client(std::time::Duration::from_secs(300))?;

    let response = client
        .post(&url)
        .headers(headers)
        .body(body_json)
        .send()
        .await
        .context("failed to connect to Anthropic API")?;

    if !response.status().is_success() {
        let status = response.status();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let text = response.text().await.unwrap_or_default();
        super::log_api_error(status, &text);
        if status.as_u16() == 429
            && let Some(kv_key) = config.credential_kv_key.as_deref()
            && let Some(until) =
                super::session_lock::parse_anthropic_lock(retry_after.as_deref(), &text)
        {
            let _ = super::session_lock::mark_locked(kv_key, until, &text);
        }
        bail!("Anthropic API error ({}): {}", status, text);
    }

    let rate_limit = RateLimitSnapshot::from_anthropic_headers(response.headers()).into_option();
    if let Some(kv_key) = config.credential_kv_key.as_deref() {
        let _ = super::session_lock::clear_locked(kv_key);
    }

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
// Request body construction
// ---------------------------------------------------------------------------

fn build_request_body(
    api_key: &str,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    config: &StreamConfig,
    is_oauth: bool,
) -> AnthropicRequest {
    // Collect all tool_use IDs from assistant messages so we can drop orphaned tool_results
    let mut tool_use_ids = HashSet::new();
    for msg in messages {
        if matches!(msg.role, Role::Assistant) {
            for block in &msg.content {
                if let ContentBlock::ToolCall { id, .. } = block {
                    tool_use_ids.insert(super::sanitize_id_anthropic(id));
                }
            }
        }
    }

    let mut api_messages: Vec<Value> = Vec::new();

    for msg in messages {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };

        // Filter out orphaned tool_result blocks
        let filtered: Vec<ContentBlock> = msg
            .content
            .iter()
            .filter(|b| {
                if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                    tool_use_ids.contains(&super::sanitize_id_anthropic(tool_use_id))
                } else {
                    true
                }
            })
            .cloned()
            .collect();

        if filtered.is_empty() {
            continue;
        }

        let content = if is_oauth {
            serialize_oauth_content(&filtered)
        } else {
            json!(serialize_content_blocks(&filtered, false))
        };

        api_messages.push(json!({ "role": role, "content": content }));
    }

    let api_tools: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "name": if is_oauth { to_claude_code_tool_name(&t.name) } else { t.name.clone() },
                "description": t.description,
                "input_schema": t.input_schema,
            })
        })
        .collect();

    let metadata = if is_oauth {
        Some(json!({
            "user_id": serde_json::to_string(&json!({
                "device_id": get_or_create_device_id(),
                "account_uuid": get_account_uuid(api_key),
                "session_id": format!("sidekar-{}", std::process::id()),
            })).unwrap_or_default()
        }))
    } else {
        None
    };

    let tools = if api_tools.is_empty() {
        None
    } else {
        Some(api_tools)
    };

    let mut request = AnthropicRequest {
        system: build_system_blocks(system_prompt, messages, is_oauth),
        model: model.to_string(),
        max_tokens: config.max_tokens,
        metadata,
        messages: api_messages,
        stream: true,
        tools,
    };
    apply_cache_control(&mut request, config);
    request
}

fn build_system_blocks(
    system_prompt: &str,
    messages: &[ChatMessage],
    is_oauth: bool,
) -> Vec<Value> {
    let mut blocks = Vec::new();

    if is_oauth {
        blocks.push(json!({
            "type": "text",
            "text": build_billing_header(messages),
        }));
        blocks.push(json!({
            "type": "text",
            "text": "You are Claude Code, Anthropic's official CLI for Claude."
        }));
    }

    if !system_prompt.is_empty() {
        blocks.push(json!({
            "type": "text",
            "text": system_prompt
        }));
    }

    blocks
}

fn ephemeral_cache_marker(config: &StreamConfig, include_scope: bool) -> Value {
    let mut marker = json!({ "type": "ephemeral" });
    if let Some(ttl) = &config.cache_ttl {
        marker["ttl"] = json!(ttl);
    }
    if include_scope && let Some(scope) = &config.cache_scope {
        marker["scope"] = json!(scope);
    }
    marker
}

fn apply_cache_control(request: &mut AnthropicRequest, config: &StreamConfig) {
    // Stable breakpoint: TTL + optional `scope` (tools/system accept scope).
    // Rolling breakpoint (latest message): TTL only — Anthropic rejects
    // `cache_control.ephemeral.scope` on message content.
    let stable_marker = ephemeral_cache_marker(config, true);
    let rolling_marker = ephemeral_cache_marker(config, false);

    // Place the stable breakpoint on the LAST TOOL definition, not on the
    // system block. Reason: Anthropic's minimum cacheable prefix is 1024
    // tokens, and our REPL system prompt (≈380 tokens including the two
    // fixed CC-identity blocks) falls below that threshold — the marker
    // would be syntactically valid but silently discarded. Placing it on
    // the last tool extends the cached prefix to system + tools (≈1830
    // tokens for the REPL's 7-tool schema), which is safely above 1024 and
    // still stable across turns (tool defs never change mid-session).
    //
    // If there are no tools, fall back to the system block and accept that
    // tiny system prompts won't cache — still beats missing the feature.
    let _ = apply_tools_cache_control(&mut request.tools, &stable_marker)
        || apply_system_cache_control(&mut request.system, &stable_marker);

    // Only stamp the LATEST message. The cache rolls forward automatically
    // because Anthropic matches the longest cached prefix from prior turns
    // on each new request. Matches Claude Code's one-marker-on-tail pattern.
    if let Some(last) = request.messages.last_mut() {
        apply_message_cache_control(last, &rolling_marker);
    }
}

fn apply_tools_cache_control(tools: &mut Option<Vec<Value>>, marker: &Value) -> bool {
    let Some(tools) = tools.as_mut() else {
        return false;
    };
    let Some(last) = tools.last_mut() else {
        return false;
    };
    last["cache_control"] = marker.clone();
    true
}

fn apply_system_cache_control(system: &mut [Value], marker: &Value) -> bool {
    for block in system.iter_mut().rev() {
        if block.get("type").and_then(|v| v.as_str()) == Some("text") {
            block["cache_control"] = marker.clone();
            return true;
        }
    }
    false
}

fn apply_message_cache_control(message: &mut Value, marker: &Value) -> bool {
    let Some(content) = message.get_mut("content") else {
        return false;
    };

    if let Some(text) = content.as_str() {
        let text = text.to_string();
        if text.is_empty() {
            return false;
        }
        *content = json!([{
            "type": "text",
            "text": text,
            "cache_control": marker,
        }]);
        return true;
    }

    let Some(parts) = content.as_array_mut() else {
        return false;
    };

    for part in parts.iter_mut().rev() {
        match part.get("type").and_then(|v| v.as_str()) {
            Some("text") => {
                let text = part.get("text").and_then(|v| v.as_str()).unwrap_or("");
                if text.is_empty() {
                    continue;
                }
                part["cache_control"] = marker.clone();
                return true;
            }
            Some("tool_result") => {
                part["cache_control"] = marker.clone();
                return true;
            }
            _ => {}
        }
    }

    false
}

fn serialize_content_blocks(blocks: &[ContentBlock], oauth: bool) -> Vec<Value> {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(json!({ "type": "text", "text": text })),
            ContentBlock::Thinking {
                thinking,
                signature,
            } => Some(json!({
                "type": "thinking",
                "thinking": thinking,
                "signature": signature,
            })),
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
                ..
            } => Some(json!({
                "type": "tool_use",
                "id": super::sanitize_id_anthropic(id),
                "name": if oauth { to_claude_code_tool_name(name) } else { name.clone() },
                "input": arguments,
            })),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some(json!({
                "type": "tool_result",
                "tool_use_id": super::sanitize_id_anthropic(tool_use_id),
                "content": content,
                "is_error": is_error,
            })),
            ContentBlock::Image {
                media_type,
                data_base64,
                ..
            } => Some(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": media_type,
                    "data": data_base64,
                }
            })),
            // Encrypted reasoning is Codex-only; skip for Anthropic.
            ContentBlock::EncryptedReasoning { .. } => None,
        })
        .collect()
}

fn serialize_oauth_content(blocks: &[ContentBlock]) -> Value {
    let text_only: Option<Vec<&str>> = blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();

    if let Some(parts) = text_only {
        return json!(parts.join("\n"));
    }

    json!(serialize_content_blocks(blocks, true))
}

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    system: Vec<Value>,
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Value>,
    messages: Vec<Value>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Value>>,
}

fn build_billing_header(messages: &[ChatMessage]) -> String {
    let fingerprint = compute_fingerprint_from_messages(messages, CLAUDE_CODE_VERSION);
    format!(
        "x-anthropic-billing-header: cc_version={CLAUDE_CODE_VERSION}.{fingerprint}; cc_entrypoint=cli; {CCH_PLACEHOLDER};"
    )
}

fn compute_fingerprint_from_messages(messages: &[ChatMessage], version: &str) -> String {
    let first_user_text = messages
        .iter()
        .find(|msg| matches!(msg.role, Role::User))
        .and_then(|msg| {
            msg.content.iter().find_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::Image { .. } => Some("[image]"),
                _ => None,
            })
        })
        .unwrap_or("");

    compute_fingerprint(first_user_text, version)
}

fn compute_fingerprint(message_text: &str, version: &str) -> String {
    let chars = [4usize, 7, 20]
        .into_iter()
        .map(|i| message_text.chars().nth(i).unwrap_or('0'))
        .collect::<String>();
    let input = format!("{FINGERPRINT_SALT}{chars}{version}");
    let hash = Sha256::digest(input.as_bytes());
    hash.iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..3]
        .to_string()
}

fn sign_request_body(body: &str) -> String {
    let cch = compute_cch(body);
    body.replacen(CCH_PLACEHOLDER, &format!("cch={cch}"), 1)
}

fn compute_cch(body: &str) -> String {
    let hash = xxh64(body.as_bytes(), CCH_SEED) & CCH_MASK;
    format!("{hash:05x}")
}

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
    let mut stop_reason = StopReason::Stop;
    let mut model_id = String::new();

    let mut current_block_type: Option<BlockType> = None;
    let mut text_accum = String::new();
    let mut thinking_accum = String::new();
    let mut thinking_signature = String::new();
    let mut tool_json_accum = String::new();
    let mut tool_id = String::new();
    let mut tool_name = String::new();

    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error reading SSE chunk")?;
        decoder.push_chunk(&chunk);

        while let Some(event) = decoder.next_event() {
            let data: Value = match super::parse_sse_json(&event) {
                Some(v) => v,
                None => continue,
            };

            match event.event_type.as_deref().unwrap_or("") {
                "message_start" => {
                    if let Some(msg) = data.get("message") {
                        model_id = msg
                            .get("model")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if let Some(u) = msg.get("usage") {
                            usage.input_tokens =
                                u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            usage.cache_read_tokens =
                                u.get("cache_read_input_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0) as u32;
                            usage.cache_write_tokens =
                                u.get("cache_creation_input_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0) as u32;
                        }
                    }
                }

                "content_block_start" => {
                    let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    if let Some(block) = data.get("content_block") {
                        match block.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                            "text" => {
                                current_block_type = Some(BlockType::Text);
                                text_accum.clear();
                            }
                            "thinking" => {
                                current_block_type = Some(BlockType::Thinking);
                                thinking_accum.clear();
                                thinking_signature.clear();
                            }
                            "tool_use" => {
                                current_block_type = Some(BlockType::ToolUse);
                                tool_json_accum.clear();
                                tool_id = block
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                tool_name = block
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let _ = tx.send(StreamEvent::ToolCallStart {
                                    index,
                                    id: tool_id.clone(),
                                    name: tool_name.clone(),
                                });
                            }
                            _ => {
                                current_block_type = None;
                            }
                        }
                    }
                }

                "content_block_delta" => {
                    let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    if let Some(delta) = data.get("delta") {
                        match delta.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                            "text_delta" => {
                                if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                                    text_accum.push_str(text);
                                    let _ = tx.send(StreamEvent::TextDelta {
                                        delta: text.to_string(),
                                    });
                                }
                            }
                            "thinking_delta" => {
                                if let Some(text) = delta.get("thinking").and_then(|v| v.as_str()) {
                                    thinking_accum.push_str(text);
                                    let _ = tx.send(StreamEvent::ThinkingDelta {
                                        delta: text.to_string(),
                                    });
                                }
                            }
                            "input_json_delta" => {
                                if let Some(json_str) =
                                    delta.get("partial_json").and_then(|v| v.as_str())
                                {
                                    tool_json_accum.push_str(json_str);
                                    let _ = tx.send(StreamEvent::ToolCallDelta {
                                        index,
                                        delta: json_str.to_string(),
                                    });
                                }
                            }
                            "signature_delta" => {
                                if let Some(sig) = delta.get("signature").and_then(|v| v.as_str()) {
                                    thinking_signature.push_str(sig);
                                }
                            }
                            _ => {}
                        }
                    }
                }

                "content_block_stop" => {
                    let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    match current_block_type {
                        Some(BlockType::Text) => {
                            content_blocks.push(ContentBlock::Text {
                                text: std::mem::take(&mut text_accum),
                            });
                        }
                        Some(BlockType::Thinking) => {
                            content_blocks.push(ContentBlock::Thinking {
                                thinking: std::mem::take(&mut thinking_accum),
                                signature: std::mem::take(&mut thinking_signature),
                            });
                        }
                        Some(BlockType::ToolUse) => {
                            let arguments =
                                serde_json::from_str(&tool_json_accum).unwrap_or(json!({}));
                            content_blocks.push(ContentBlock::ToolCall {
                                id: std::mem::take(&mut tool_id),
                                name: std::mem::take(&mut tool_name),
                                arguments,
                                thought_signature: None,
                            });
                            let _ = tx.send(StreamEvent::ToolCallEnd { index });
                        }
                        None => {}
                    }
                    current_block_type = None;
                }

                "message_delta" => {
                    if let Some(delta) = data.get("delta") {
                        stop_reason = match delta
                            .get("stop_reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("end_turn")
                        {
                            "end_turn" | "pause_turn" | "stop_sequence" => StopReason::Stop,
                            "max_tokens" => StopReason::Length,
                            "tool_use" => StopReason::ToolUse,
                            _ => StopReason::Error,
                        };
                    }
                    if let Some(u) = data.get("usage")
                        && let Some(v) = u.get("output_tokens").and_then(|v| v.as_u64())
                    {
                        usage.output_tokens = v as u32;
                    }
                }

                "message_stop" => {
                    let _ = tx.send(StreamEvent::Done {
                        message: AssistantResponse {
                            content: std::mem::take(&mut content_blocks),
                            usage: usage.clone(),
                            stop_reason: stop_reason.clone(),
                            model: model_id.clone(),
                            response_id: String::new(),
                            rate_limit: rate_limit.clone(),
                        },
                    });
                }

                "error" => {
                    let msg = data
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown API error");
                    let _ = tx.send(StreamEvent::Error {
                        message: msg.to_string(),
                    });
                }

                _ => {}
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum BlockType {
    Text,
    Thinking,
    ToolUse,
}

fn to_claude_code_tool_name(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "bash" => "Bash".to_string(),
        "read" => "Read".to_string(),
        "write" => "Write".to_string(),
        "edit" => "Edit".to_string(),
        "glob" => "Glob".to_string(),
        "grep" => "Grep".to_string(),
        _ => name.to_string(),
    }
}

fn get_or_create_device_id() -> String {
    const KEY: &str = "internal:device_id";
    if let Ok(Some(entry)) = crate::broker::kv_get(KEY) {
        return entry.value;
    }
    let mut bytes = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
    let id = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let _ = crate::broker::kv_set(KEY, &id, None);
    id
}

fn get_account_uuid(api_key: &str) -> String {
    // OAuth creds live under nicknamed keys (`oauth:claude-kb`,
    // `oauth:claude-ks`, etc.) — not the fixed `oauth:anthropic` key. Scan
    // all kv entries, find the one whose stored access_token matches the
    // one we're about to send, and pull `metadata.account_uuid` from it.
    //
    // This matters because `scope: "global"` cache reuse is keyed by
    // account_uuid server-side. An empty account_uuid silently disables
    // global caching — which is the bug that kept REPL cache_creation
    // stuck at 0 even though syntactically the request looked identical
    // to Claude Code's.
    let Ok(entries) = crate::broker::kv_list(None) else {
        return String::new();
    };
    for entry in entries {
        if !entry.key.starts_with("oauth:") {
            continue;
        }
        let Ok(creds) = serde_json::from_str::<serde_json::Value>(&entry.value) else {
            continue;
        };
        if creds.get("access_token").and_then(|v| v.as_str()) != Some(api_key) {
            continue;
        }
        if let Some(uuid) = creds
            .get("metadata")
            .and_then(|m| m.get("account_uuid"))
            .and_then(|v| v.as_str())
        {
            return uuid.to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests;
