use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::sync::mpsc;

use super::{
    AssistantResponse, ChatMessage, ContentBlock, Role, StopReason, StreamEvent, ToolDef, Usage,
};

/// Non-streaming call to the OpenAI Codex Responses API.
pub async fn stream(
    api_key: &str,
    account_id: &str,
    base_url: &str,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    prompt_cache_key: Option<&str>,
) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
    let body = build_request_body(model, system_prompt, messages, tools, prompt_cache_key);
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

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

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
// Request body
// ---------------------------------------------------------------------------

fn build_request_body(
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    prompt_cache_key: Option<&str>,
) -> Value {
    let mut input: Vec<Value> = Vec::new();

    for msg in messages {
        match msg.role {
            Role::User => {
                // User text messages
                let text = msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                if !text.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "user",
                        "content": text,
                    }));
                }

                // Tool results
                for block in &msg.content {
                    if let ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } = block
                    {
                        let (call_id, _) = split_tool_call_ids(tool_use_id);
                        input.push(json!({
                            "type": "function_call_output",
                            "call_id": call_id,
                            "output": content,
                        }));
                    }
                }
            }
            Role::Assistant => {
                // Text output
                let text = msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

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

    let mut body = json!({
        "model": model,
        "instructions": system_prompt,
        "input": input,
        "stream": true,
        "store": false,
    });

    if let Some(key) = prompt_cache_key.filter(|key| !key.is_empty()) {
        body["prompt_cache_key"] = json!(key);
    }

    if !api_tools.is_empty() {
        body["tools"] = json!(api_tools);
        body["tool_choice"] = json!("auto");
    }

    body
}

// ---------------------------------------------------------------------------
// SSE stream parsing
// ---------------------------------------------------------------------------

async fn parse_sse_stream(
    response: reqwest::Response,
    tx: &mpsc::UnboundedSender<StreamEvent>,
) -> Result<()> {
    let mut stream = response.bytes_stream();
    let mut decoder = super::SseDecoder::new();

    let mut content_blocks: Vec<ContentBlock> = Vec::new();
    let mut usage = Usage::default();
    let mut model_id = String::new();
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
                    if let Some(delta) = data.get("delta").and_then(|v| v.as_str()) {
                        if !delta.is_empty() {
                            let _ = tx.send(StreamEvent::TextDelta {
                                delta: delta.to_string(),
                            });
                        }
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
                    if let Some(text) = data.get("text").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            content_blocks.push(ContentBlock::Text {
                                text: text.to_string(),
                            });
                        }
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
                        });
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
                    }

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
                        .unwrap_or("Unknown Codex error");
                    let _ = tx.send(StreamEvent::Error {
                        message: msg.to_string(),
                    });
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
    if let Some(event_id) = get_event_call_id(data) {
        if pending_tool_calls.contains_key(&event_id) {
            return pending_tool_calls.get_mut(&event_id);
        }
    }

    if pending_tool_calls.len() == 1 {
        let only_key = pending_tool_calls.keys().next()?.to_string();
        return pending_tool_calls.get_mut(&only_key);
    }

    None
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

#[cfg(test)]
mod tests {
    use super::{apply_usage, build_request_body};
    use crate::providers::{ChatMessage, ContentBlock, Role, Usage};
    use serde_json::json;

    #[test]
    fn build_request_body_includes_prompt_cache_key() {
        let body = build_request_body(
            "gpt-5.4",
            "system",
            &[ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "hello".to_string(),
                }],
            }],
            &[],
            Some("sess-123"),
        );

        assert_eq!(body.get("prompt_cache_key").and_then(|v| v.as_str()), Some("sess-123"));
    }

    #[test]
    fn apply_usage_extracts_cached_token_details() {
        let usage_json = json!({
            "input_tokens": 1200,
            "output_tokens": 55,
            "input_tokens_details": {
                "cached_tokens": 400,
                "cache_creation_tokens": 100
            }
        });
        let mut usage = Usage::default();

        apply_usage(&usage_json, &mut usage);

        assert_eq!(usage.input_tokens, 700);
        assert_eq!(usage.output_tokens, 55);
        assert_eq!(usage.cache_read_tokens, 400);
        assert_eq!(usage.cache_write_tokens, 100);
    }
}
