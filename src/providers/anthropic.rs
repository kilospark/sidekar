use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};
use futures_util::{StreamExt, pin_mut};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{
    AssistantResponse, ChatMessage, ContentBlock, RateLimitSnapshot, Role, StopReason,
    StreamConfig, StreamEvent, ToolDef, Usage,
};

const CLAUDE_CODE_VERSION: &str = "2.1.87";

/// Rewrite messages so user/tool image payloads become text placeholders (matches OpenAI-compat
/// path in `openrouter.rs`). Needed for gateways (e.g. OpenCode Go) that translate Anthropic
/// multimodal payloads to OpenAI upstreams that reject `image_url`.
fn omit_multimodal_user_images_from_messages(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    messages
        .iter()
        .map(|m| ChatMessage {
            role: m.role.clone(),
            content: omit_multimodal_user_images_from_blocks(&m.content),
        })
        .collect()
}

fn omit_multimodal_user_images_from_blocks(blocks: &[ContentBlock]) -> Vec<ContentBlock> {
    blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Image {
                media_type,
                source_path,
                ..
            } => ContentBlock::Text {
                text: match source_path {
                    Some(path) => format!(
                        "[Image omitted: model does not support vision input ({media_type}, {path})]"
                    ),
                    None => format!(
                        "[Image omitted: model does not support vision input ({media_type})]"
                    ),
                },
            },
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                content_images,
            } if content_images.is_empty() => ContentBlock::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: content.clone(),
                is_error: *is_error,
                content_images: Vec::new(),
            },
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                content_images,
            } => {
                let n = content_images.len();
                let suffix = if n == 1 {
                    "\n\n[1 image omitted: model does not support vision input.]".to_string()
                } else {
                    format!("\n\n[{n} images omitted: model does not support vision input.]")
                };
                ContentBlock::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content: format!("{content}{suffix}"),
                    is_error: *is_error,
                    content_images: Vec::new(),
                }
            }
            ContentBlock::Text { .. }
            | ContentBlock::Thinking { .. }
            | ContentBlock::ToolCall { .. }
            | ContentBlock::EncryptedReasoning { .. }
            | ContentBlock::Reasoning { .. } => b.clone(),
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn stream_messages_once(
    api_key: &str,
    base_url: &str,
    model_display: &str,
    enable_1m_beta: bool,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    config: &StreamConfig,
    is_oauth: bool,
) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
    let body = build_request_body(
        api_key,
        model_display,
        system_prompt,
        messages,
        tools,
        config,
        is_oauth,
    );
    let body_json = serde_json::to_string(&body)?;
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
        // permitted`.
        String::from(
            "oauth-2025-04-20,fine-grained-tool-streaming-2025-05-14,prompt-caching-scope-2026-01-05",
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
        headers.insert("user-agent", format!("claude-cli/{CLAUDE_CODE_VERSION}").parse()?);
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

/// Stream a response from the Anthropic Messages API (`/v1/messages`).
///
/// When `allow_vision_from_catalog` is false — or when the upstream returns vision-deserialize
/// errors despite `true` — user/tool images are rewritten to placeholders before JSON is sent so
/// OpenCode-style gateways can forward to strict text-only OpenAI backends (e.g. DeepSeek via Go).
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
    provider_type: &str,
    allow_vision_from_catalog: bool,
) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
    let is_oauth = api_key.contains("sk-ant-oat");

    let (model_display, enable_1m_beta) = match model.strip_suffix(super::ANTHROPIC_1M_SUFFIX) {
        Some(clean) => (clean, true),
        None => (model, false),
    };

    if !allow_vision_from_catalog {
        let stripped = omit_multimodal_user_images_from_messages(messages);
        return stream_messages_once(
            api_key,
            base_url,
            model_display,
            enable_1m_beta,
            system_prompt,
            &stripped,
            tools,
            config,
            is_oauth,
        )
        .await;
    }

    match stream_messages_once(
        api_key,
        base_url,
        model_display,
        enable_1m_beta,
        system_prompt,
        messages,
        tools,
        config,
        is_oauth,
    )
    .await
    {
        Ok(rx) => Ok(rx),
        Err(e) if super::capabilities::is_vision_rejection_error(&e.to_string()) => {
            super::capabilities::record_vision_rejection(provider_type, model);
            crate::broker::try_log_event(
                "debug",
                "provider",
                "vision-rejected-retry-text-only-messages-api",
                Some(&format!("provider={provider_type} model={model}")),
            );
            let stripped = omit_multimodal_user_images_from_messages(messages);
            stream_messages_once(
                api_key,
                base_url,
                model_display,
                enable_1m_beta,
                system_prompt,
                &stripped,
                tools,
                config,
                is_oauth,
            )
            .await
        }
        Err(e) => Err(e),
    }
}

/// Stream Claude via Vertex AI partner REST `:streamRawPredict` (Anthropic Messages JSON body).
///
/// `base_url` should look like:
/// `https://REGION-aiplatform.googleapis.com/v1/projects/PROJECT/locations/REGION/publishers/anthropic/models/MODEL`
/// optionally ending in `:rawPredict` or `:streamRawPredict`.
///
/// Auth: GCP OAuth bearer token (`Authorization: Bearer`) plus `x-goog-user-project`.
#[allow(clippy::too_many_arguments)]
pub async fn stream_vertex_anthropic_partner(
    api_key: &str,
    base_url: &str,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    config: &StreamConfig,
) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
    let url = super::vertex::anthropic_partner_stream_url(base_url);

    let (picker_clean, enable_1m_beta) = match model.strip_suffix(super::ANTHROPIC_1M_SUFFIX) {
        Some(clean) => (clean, true),
        None => (model, false),
    };
    let phantom_model = super::vertex::anthropic_partner_model_id(base_url).unwrap_or_else(|| {
        strip_vertex_partner_model_id(picker_clean).unwrap_or_else(|| picker_clean.to_string())
    });

    let mut cfg = config.clone();
    cfg.suppress_anthropic_cache_markers = true;

    let body = build_request_body(
        "",
        &phantom_model,
        system_prompt,
        messages,
        tools,
        &cfg,
        false,
    );
    let mut body_val = serde_json::to_value(&body)?;
    if let Some(obj) = body_val.as_object_mut() {
        obj.remove("model");
        obj.insert("anthropic_version".to_string(), json!("vertex-2023-10-16"));
    }
    let body_json = serde_json::to_string(&body_val)?;

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("content-type", "application/json".parse()?);
    headers.insert("authorization", format!("Bearer {api_key}").parse()?);

    let mut beta_parts: Vec<&'static str> = Vec::new();
    if !tools.is_empty() {
        beta_parts.push("fine-grained-tool-streaming-2025-05-14");
    }
    if enable_1m_beta {
        beta_parts.push("context-1m-2025-08-07");
    }
    if !beta_parts.is_empty() {
        headers.insert("anthropic-beta", beta_parts.join(",").parse()?);
    }

    if let Some(project) = super::vertex::extract_project(base_url)
        && let Ok(value) = project.parse()
    {
        headers.insert("x-goog-user-project", value);
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
        .context("failed to connect to Vertex Claude API")?;

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
        bail!("Vertex Claude API error ({}): {}", status, text);
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

fn strip_vertex_partner_model_id(model: &str) -> Option<String> {
    let id = model.rsplit_once('/').map(|(_, id)| id).unwrap_or(model);
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
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
        system: build_system_blocks(system_prompt),
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

fn build_system_blocks(system_prompt: &str) -> Vec<Value> {
    if system_prompt.is_empty() {
        return Vec::new();
    }
    vec![json!({
        "type": "text",
        "text": system_prompt
    })]
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
    if config.suppress_anthropic_cache_markers {
        return;
    }
    // Stable breakpoint: TTL + optional `scope` (tools/system accept scope).
    // Rolling breakpoint (latest message): TTL only — Anthropic rejects
    // `cache_control.ephemeral.scope` on message content.
    let stable_marker = ephemeral_cache_marker(config, true);
    let rolling_marker = ephemeral_cache_marker(config, false);

    // Place the stable breakpoint on the LAST TOOL definition, not on the
    // system block. Reason: Anthropic's minimum cacheable prefix is 1024
    // tokens, and a typical REPL system prompt falls below that threshold —
    // the marker would be syntactically valid but silently discarded. Placing it on
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
                content_images,
            } => {
                let content_wire = if content_images.is_empty() {
                    json!(content)
                } else {
                    let mut parts: Vec<Value> = vec![json!({
                        "type": "text",
                        "text": content,
                    })];
                    for img in content_images {
                        parts.push(json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": img.media_type,
                                "data": img.data_base64,
                            }
                        }));
                    }
                    json!(parts)
                };
                Some(json!({
                    "type": "tool_result",
                    "tool_use_id": super::sanitize_id_anthropic(tool_use_id),
                    "content": content_wire,
                    "is_error": is_error,
                }))
            }
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
            // OpenAI-compat reasoning replay; skip on Anthropic.
            ContentBlock::Reasoning { .. } => None,
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

pub(super) fn build_bedrock_anthropic_messages_request_body(
    bedrock_model_id: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    config: &StreamConfig,
) -> Result<Vec<u8>> {
    let req = build_request_body(
        "",
        bedrock_model_id,
        system_prompt,
        messages,
        tools,
        config,
        false,
    );
    let mut v = serde_json::to_value(&req)?;
    if let Some(obj) = v.as_object_mut() {
        obj.remove("model");
        obj.remove("stream"); // InvokeModelWithResponseStream — streaming via route, not body
        obj.insert("anthropic_version".to_string(), json!("bedrock-2023-05-31"));
    }
    Ok(serde_json::to_vec(&v)?)
}

// ---------------------------------------------------------------------------
async fn parse_sse_stream(
    response: reqwest::Response,
    rate_limit: Option<RateLimitSnapshot>,
    tx: &mpsc::UnboundedSender<StreamEvent>,
) -> Result<()> {
    let stream = response
        .bytes_stream()
        .map(|r| r.map_err(anyhow::Error::from));
    parse_sse_bytes_stream(stream, rate_limit, tx).await
}

struct AnthropicStreamState {
    content_blocks: Vec<ContentBlock>,
    usage: Usage,
    stop_reason: StopReason,
    model_id: String,
    pending_blocks: HashMap<usize, PendingBlock>,
}

impl AnthropicStreamState {
    fn new() -> Self {
        Self {
            content_blocks: Vec::new(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            model_id: String::new(),
            pending_blocks: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
enum PendingBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    ToolUse {
        id: String,
        name: String,
        initial_input: Option<Value>,
        input_json: String,
    },
}

fn is_empty_tool_input(value: &Value) -> bool {
    matches!(value, Value::Object(map) if map.is_empty())
}

fn parsed_tool_input_score(value: &Value) -> usize {
    match value {
        Value::Object(map) => 3 + map.len(),
        Value::Array(items) => 2 + items.len(),
        Value::Null => 0,
        _ => 1,
    }
}

fn resolve_anthropic_tool_input(initial_input: Option<Value>, input_json: &str) -> Value {
    let mut best: Option<(usize, Value)> = None;

    if let Some(value) = initial_input
        && !is_empty_tool_input(&value)
    {
        let raw_len = serde_json::to_string(&value).map(|s| s.len()).unwrap_or(0);
        best = Some(((parsed_tool_input_score(&value) * 1_000) + raw_len, value));
    }

    let trimmed = input_json.trim();
    if !trimmed.is_empty() {
        if let Ok(value) = serde_json::from_str::<Value>(trimmed)
            && !is_empty_tool_input(&value)
        {
            let score = (parsed_tool_input_score(&value) * 1_000) + trimmed.len();
            let replace = best
                .as_ref()
                .map(|(best_score, _)| score > *best_score)
                .unwrap_or(true);
            if replace {
                best = Some((score, value));
            }
        } else {
            let prefix: String = trimmed.chars().take(500).collect();
            crate::broker::try_log_error(
                "anthropic-transport",
                "tool_use input_json parse failed",
                Some(&format!("args_prefix={prefix}")),
            );
        }
    }

    best.map(|(_, value)| value).unwrap_or_else(|| json!({}))
}

fn handle_anthropic_event(
    event_type: &str,
    data: &Value,
    rate_limit: &Option<RateLimitSnapshot>,
    tx: &mpsc::UnboundedSender<StreamEvent>,
    state: &mut AnthropicStreamState,
) {
    match event_type {
        "message_start" => {
            if let Some(msg) = data.get("message") {
                state.model_id = msg
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(u) = msg.get("usage") {
                    state.usage.input_tokens =
                        u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    state.usage.cache_read_tokens = u
                        .get("cache_read_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    state.usage.cache_write_tokens = u
                        .get("cache_creation_input_tokens")
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
                        state.pending_blocks.insert(
                            index,
                            PendingBlock::Text {
                                text: String::new(),
                            },
                        );
                    }
                    "thinking" => {
                        state.pending_blocks.insert(
                            index,
                            PendingBlock::Thinking {
                                thinking: String::new(),
                                signature: String::new(),
                            },
                        );
                    }
                    "tool_use" => {
                        let tool_id = block
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let tool_name = block
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        state.pending_blocks.insert(
                            index,
                            PendingBlock::ToolUse {
                                id: tool_id.clone(),
                                name: tool_name.clone(),
                                initial_input: block.get("input").cloned(),
                                input_json: String::new(),
                            },
                        );
                        let _ = tx.send(StreamEvent::ToolCallStart {
                            index,
                            id: tool_id,
                            name: tool_name,
                        });
                    }
                    _ => {}
                }
            }
        }

        "content_block_delta" => {
            let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            if let Some(delta) = data.get("delta") {
                match delta.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                    "text_delta" => {
                        if let Some(text) = delta.get("text").and_then(|v| v.as_str())
                            && let Some(PendingBlock::Text { text: accum }) =
                                state.pending_blocks.get_mut(&index)
                        {
                            accum.push_str(text);
                            let _ = tx.send(StreamEvent::TextDelta {
                                delta: text.to_string(),
                            });
                        }
                    }
                    "thinking_delta" => {
                        if let Some(text) = delta.get("thinking").and_then(|v| v.as_str())
                            && let Some(PendingBlock::Thinking {
                                thinking: accum, ..
                            }) = state.pending_blocks.get_mut(&index)
                        {
                            accum.push_str(text);
                            let _ = tx.send(StreamEvent::ThinkingDelta {
                                delta: text.to_string(),
                            });
                        }
                    }
                    "input_json_delta" => {
                        if let Some(json_str) = delta.get("partial_json").and_then(|v| v.as_str())
                            && let Some(PendingBlock::ToolUse {
                                input_json: accum, ..
                            }) = state.pending_blocks.get_mut(&index)
                        {
                            accum.push_str(json_str);
                            let _ = tx.send(StreamEvent::ToolCallDelta {
                                index,
                                delta: json_str.to_string(),
                            });
                        }
                    }
                    "signature_delta" => {
                        if let Some(sig) = delta.get("signature").and_then(|v| v.as_str())
                            && let Some(PendingBlock::Thinking { signature, .. }) =
                                state.pending_blocks.get_mut(&index)
                        {
                            signature.push_str(sig);
                        }
                    }
                    _ => {}
                }
            }
        }

        "content_block_stop" => {
            let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            match state.pending_blocks.remove(&index) {
                Some(PendingBlock::Text { text }) => {
                    state.content_blocks.push(ContentBlock::Text { text });
                }
                Some(PendingBlock::Thinking {
                    thinking,
                    signature,
                }) => {
                    state.content_blocks.push(ContentBlock::Thinking {
                        thinking,
                        signature,
                    });
                }
                Some(PendingBlock::ToolUse {
                    id,
                    name,
                    initial_input,
                    input_json,
                }) => {
                    let arguments = resolve_anthropic_tool_input(initial_input, &input_json);
                    state.content_blocks.push(ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                        thought_signature: None,
                    });
                    let _ = tx.send(StreamEvent::ToolCallEnd { index });
                }
                None => {}
            }
        }

        "message_delta" => {
            if let Some(delta) = data.get("delta") {
                state.stop_reason = match delta
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
                state.usage.output_tokens = v as u32;
            }
        }

        "message_stop" => {
            let _ = tx.send(StreamEvent::Done {
                message: AssistantResponse {
                    content: std::mem::take(&mut state.content_blocks),
                    usage: state.usage.clone(),
                    stop_reason: state.stop_reason.clone(),
                    model: state.model_id.clone(),
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

pub(super) async fn parse_sse_bytes_stream<S>(
    stream: S,
    rate_limit: Option<RateLimitSnapshot>,
    tx: &mpsc::UnboundedSender<StreamEvent>,
) -> Result<()>
where
    S: futures_util::Stream<Item = std::result::Result<bytes::Bytes, anyhow::Error>> + Send,
{
    pin_mut!(stream);
    let mut decoder = super::SseDecoder::new();
    let mut total_sse_bytes = 0usize;
    let mut parsed_events = 0usize;
    let mut state = AnthropicStreamState::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error reading SSE chunk")?;
        total_sse_bytes += chunk.len();
        decoder.push_chunk(&chunk);

        while let Some(event) = decoder.next_event() {
            parsed_events += 1;
            let data: Value = match super::parse_sse_json(&event) {
                Some(v) => v,
                None => continue,
            };
            handle_anthropic_event(
                event.event_type.as_deref().unwrap_or(""),
                &data,
                &rate_limit,
                tx,
                &mut state,
            );
        }
    }

    if decoder.unread_len() > 0 {
        bail!(
            "SSE stream ended with unread trailing bytes (bytes {}, events {}, trailing {} bytes): {}",
            total_sse_bytes,
            parsed_events,
            decoder.unread_len(),
            decoder.unread_preview(256)
        );
    }

    Ok(())
}

pub(super) async fn parse_json_event_bytes_stream<S>(
    stream: S,
    rate_limit: Option<RateLimitSnapshot>,
    tx: &mpsc::UnboundedSender<StreamEvent>,
) -> Result<()>
where
    S: futures_util::Stream<Item = std::result::Result<bytes::Bytes, anyhow::Error>> + Send,
{
    pin_mut!(stream);
    let mut total_bytes = 0usize;
    let mut parsed_events = 0usize;
    let mut state = AnthropicStreamState::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error reading Bedrock JSON event chunk")?;
        total_bytes += chunk.len();
        let data: Value = serde_json::from_slice(&chunk).with_context(|| {
            format!(
                "invalid Bedrock JSON event after {} bytes / {} events: {}",
                total_bytes,
                parsed_events,
                String::from_utf8_lossy(&chunk)
            )
        })?;
        let event_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
        parsed_events += 1;
        handle_anthropic_event(event_type, &data, &rate_limit, tx, &mut state);
    }

    Ok(())
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
