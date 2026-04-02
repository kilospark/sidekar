use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use xxhash_rust::xxh64::xxh64;

use super::{
    AssistantResponse, ChatMessage, ContentBlock, Role, StopReason, StreamEvent, ToolDef, Usage,
};

const CLAUDE_CODE_VERSION: &str = "2.1.87";
const FINGERPRINT_SALT: &str = "59cf53e54c78";
const CCH_PLACEHOLDER: &str = "cch=00000";
const CCH_SEED: u64 = 0x6E52_736A_C806_831E;
const CCH_MASK: u64 = 0xF_FFFF;

/// Stream a response from the Anthropic Messages API.
pub async fn stream(
    api_key: &str,
    base_url: &str,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
    let is_oauth = api_key.contains("sk-ant-oat");

    let max_tokens = super::model_info(model)
        .map(|m| m.max_output)
        .unwrap_or(16_000);

    let body = build_request_body(model, system_prompt, messages, tools, max_tokens, is_oauth);
    let mut body_json = serde_json::to_string(&body)?;
    if is_oauth && body_json.contains(CCH_PLACEHOLDER) {
        body_json = sign_request_body(&body_json);
    }
    let url = format!("{}/v1/messages", base_url.trim_end_matches('/'));

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("content-type", "application/json".parse()?);
    headers.insert("accept", "application/json".parse()?);
    headers.insert("anthropic-version", "2023-06-01".parse()?);
    headers.insert("anthropic-dangerous-direct-browser-access", "true".parse()?);

    let needs_interleaved_beta =
        !(model.contains("opus-4") || (model.contains("sonnet-4") && model.contains("4-6")));

    if is_oauth {
        let mut beta_features = vec!["fine-grained-tool-streaming-2025-05-14"];
        if needs_interleaved_beta {
            beta_features.push("interleaved-thinking-2025-05-14");
        }
        headers.insert(
            "anthropic-beta",
            format!(
                "claude-code-20250219,oauth-2025-04-20,{}",
                beta_features.join(",")
            )
            .parse()?,
        );
        headers.insert("authorization", format!("Bearer {api_key}").parse()?);
        headers.insert("user-agent", "claude-cli/2.1.87".parse()?);
        headers.insert("x-app", "cli".parse()?);
    } else {
        let mut beta_features = vec!["fine-grained-tool-streaming-2025-05-14"];
        if needs_interleaved_beta {
            beta_features.push("interleaved-thinking-2025-05-14");
        }
        headers.insert("anthropic-beta", beta_features.join(",").parse()?);
        headers.insert("x-api-key", api_key.parse()?);
    }

    if let Ok(log_body) = serde_json::from_str::<Value>(&body_json) {
        super::log_api_request(&url, &headers, &log_body);
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    let response = client
        .post(&url)
        .headers(headers)
        .body(body_json)
        .send()
        .await
        .context("failed to connect to Anthropic API")?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        super::log_api_error(status, &text);
        bail!("Anthropic API error ({}): {}", status, text);
    }

    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        if let Err(e) = parse_sse_stream(response, &tx).await {
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
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    max_tokens: u32,
    is_oauth: bool,
) -> AnthropicRequest {
    let mut api_messages: Vec<Value> = Vec::new();

    for msg in messages {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };

        let content = if is_oauth {
            serialize_oauth_content(&msg.content)
        } else {
            json!(serialize_content_blocks(&msg.content, false))
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

    // Thinking config
    let supports_thinking = super::model_info(model)
        .map(|m| m.supports_thinking)
        .unwrap_or(false);

    let is_adaptive =
        model.contains("opus-4") || (model.contains("sonnet-4") && model.contains("4-6"));
    let metadata = if is_oauth {
        Some(json!({
            "user_id": serde_json::to_string(&json!({
                "device_id": get_or_create_device_id(),
                "account_uuid": get_account_uuid(),
                "session_id": format!("sidekar-{}", std::process::id()),
            })).unwrap_or_default()
        }))
    } else {
        None
    };

    let thinking = if supports_thinking {
        Some(if is_adaptive {
            json!({ "type": "adaptive" })
        } else {
            json!({ "type": "enabled", "budget_tokens": 10000 })
        })
    } else {
        None
    };
    let temperature = if supports_thinking { None } else { Some(1.0) };
    let tools = if api_tools.is_empty() {
        None
    } else {
        Some(api_tools)
    };

    AnthropicRequest {
        system: build_system_blocks(system_prompt, messages, is_oauth),
        model: model.to_string(),
        max_tokens,
        thinking,
        temperature,
        metadata,
        messages: api_messages,
        stream: true,
        tools,
    }
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

fn serialize_content_blocks(blocks: &[ContentBlock], oauth: bool) -> Vec<Value> {
    blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => json!({ "type": "text", "text": text }),
            ContentBlock::Thinking {
                thinking,
                signature,
            } => json!({
                "type": "thinking",
                "thinking": thinking,
                "signature": signature,
            }),
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => json!({
                "type": "tool_use",
                "id": id,
                "name": if oauth { to_claude_code_tool_name(name) } else { name.clone() },
                "input": arguments,
            }),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
                "is_error": is_error,
            }),
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
    thinking: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
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
                    if let Some(u) = data.get("usage") {
                        if let Some(v) = u.get("output_tokens").and_then(|v| v.as_u64()) {
                            usage.output_tokens = v as u32;
                        }
                    }
                }

                "message_stop" => {
                    let _ = tx.send(StreamEvent::Done {
                        message: AssistantResponse {
                            content: std::mem::take(&mut content_blocks),
                            usage: usage.clone(),
                            stop_reason: stop_reason.clone(),
                            model: model_id.clone(),
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
    let _ = crate::broker::kv_set(KEY, &id);
    id
}

fn get_account_uuid() -> String {
    crate::broker::kv_get(super::oauth::KV_KEY_ANTHROPIC)
        .ok()
        .flatten()
        .and_then(|entry| serde_json::from_str::<serde_json::Value>(&entry.value).ok())
        .and_then(|creds| {
            creds
                .get("metadata")?
                .get("account_uuid")?
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{CCH_PLACEHOLDER, compute_fingerprint, sign_request_body};

    #[test]
    fn fingerprint_matches_reference_example() {
        let prompt = "Say 'hello' and nothing else.";
        assert_eq!(compute_fingerprint(prompt, "2.1.37"), "9e7");
    }

    #[test]
    fn sign_request_body_replaces_only_the_first_placeholder() {
        let body =
            format!("{{\"system\":\"{CCH_PLACEHOLDER}\",\"messages\":\"{CCH_PLACEHOLDER}\"}}");
        let signed = sign_request_body(&body);

        assert!(!signed.contains(&format!("\"system\":\"{CCH_PLACEHOLDER}\"")));
        assert!(signed.contains(&format!("\"messages\":\"{CCH_PLACEHOLDER}\"")));
    }
}
