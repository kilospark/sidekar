use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{
    AssistantResponse, ChatMessage, ContentBlock, Role, StopReason, StreamEvent, ToolDef, Usage,
};

/// Streaming call to OpenRouter's OpenAI-compatible chat completions API.
pub async fn stream(
    api_key: &str,
    base_url: &str,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
    let body = build_request_body(model, system_prompt, messages, tools);
    let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("content-type", "application/json".parse()?);
    headers.insert("authorization", format!("Bearer {api_key}").parse()?);

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
        .context("failed to connect to OpenRouter API")?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        super::log_api_error(status, &text);
        bail!("OpenRouter API error ({}): {}", status, text);
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
// Request body (OpenAI chat completions format)
// ---------------------------------------------------------------------------

fn build_request_body(
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
                // Collect text blocks
                let text: String = msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                if !text.is_empty() {
                    api_messages.push(json!({
                        "role": "user",
                        "content": text,
                    }));
                }

                // Tool results → separate "tool" role messages
                for block in &msg.content {
                    if let ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } = block
                    {
                        api_messages.push(json!({
                            "role": "tool",
                            "tool_call_id": super::sanitize_id_openai(tool_use_id),
                            "content": content,
                        }));
                    }
                }
            }
            Role::Assistant => {
                let text: String = msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                let tool_calls: Vec<Value> = msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
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

    body
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

async fn parse_sse_stream(
    response: reqwest::Response,
    tx: &mpsc::UnboundedSender<StreamEvent>,
) -> Result<()> {
    let mut stream = response.bytes_stream();
    let mut decoder = super::SseDecoder::new();

    let mut content_blocks: Vec<ContentBlock> = Vec::new();
    let mut text_buf = String::new();
    let mut usage = Usage::default();
    let mut model_id = String::new();
    let mut finish_reason: Option<String> = None;
    let mut pending_tool_calls: Vec<PendingToolCall> = Vec::new();

    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error reading SSE chunk")?;
        decoder.push_chunk(&chunk);

        while let Some(event) = decoder.next_event() {
            let data: Value = match super::parse_sse_json(&event) {
                Some(v) => v,
                None => continue,
            };

            // Extract model from first chunk
            if model_id.is_empty() {
                if let Some(m) = data.get("model").and_then(|v| v.as_str()) {
                    model_id = m.to_string();
                }
            }

            // Usage (present in final chunk when stream_options.include_usage is set)
            if let Some(u) = data.get("usage") {
                usage.input_tokens =
                    u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                usage.output_tokens = u
                    .get("completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
            }

            let choices = match data.get("choices").and_then(|v| v.as_array()) {
                Some(c) => c,
                None => continue,
            };

            for choice in choices {
                // Check finish_reason
                if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                    finish_reason = Some(fr.to_string());
                }

                let delta = match choice.get("delta") {
                    Some(d) => d,
                    None => continue,
                };

                // Text content delta
                if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                    if !content.is_empty() {
                        text_buf.push_str(content);
                        let _ = tx.send(StreamEvent::TextDelta {
                            delta: content.to_string(),
                        });
                    }
                }

                // Tool call deltas
                if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tool_calls {
                        let tc_index =
                            tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

                        // New tool call (has id and function.name)
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

                            // Ensure vec is large enough
                            while pending_tool_calls.len() <= tc_index {
                                pending_tool_calls.push(PendingToolCall {
                                    id: String::new(),
                                    name: String::new(),
                                    arguments: String::new(),
                                    index: pending_tool_calls.len(),
                                });
                            }
                            pending_tool_calls[tc_index] = PendingToolCall {
                                id: id.to_string(),
                                name: name.clone(),
                                arguments: initial_args,
                                index: tc_index,
                            };

                            let _ = tx.send(StreamEvent::ToolCallStart {
                                index: tc_index,
                                id: id.to_string(),
                                name,
                            });
                        } else if let Some(args_delta) = tc
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|v| v.as_str())
                        {
                            // Argument delta for existing tool call
                            if tc_index < pending_tool_calls.len() {
                                pending_tool_calls[tc_index].arguments.push_str(args_delta);
                            }
                            let _ = tx.send(StreamEvent::ToolCallDelta {
                                index: tc_index,
                                delta: args_delta.to_string(),
                            });
                        }
                    }
                }
            }
        }
    }

    // Finalize: build content blocks from accumulated state
    if !text_buf.is_empty() {
        content_blocks.push(ContentBlock::Text { text: text_buf });
    }

    for tc in &pending_tool_calls {
        if tc.id.is_empty() {
            continue;
        }
        let _ = tx.send(StreamEvent::ToolCallEnd { index: tc.index });
        let arguments: Value = serde_json::from_str(&tc.arguments).unwrap_or(json!({}));
        content_blocks.push(ContentBlock::ToolCall {
            id: tc.id.clone(),
            name: tc.name.clone(),
            arguments,
        });
    }

    let stop = match finish_reason.as_deref() {
        Some("tool_calls") => StopReason::ToolUse,
        Some("length") => StopReason::Length,
        _ => {
            if pending_tool_calls.iter().any(|tc| !tc.id.is_empty()) {
                StopReason::ToolUse
            } else {
                StopReason::Stop
            }
        }
    };

    let _ = tx.send(StreamEvent::Done {
        message: AssistantResponse {
            content: content_blocks,
            usage,
            stop_reason: stop,
            model: model_id,
        },
    });

    Ok(())
}
