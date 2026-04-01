use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
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
) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
    let body = build_request_body(model, system_prompt, messages, tools);
    let url = format!("{}/codex/responses", base_url.trim_end_matches('/'));

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("content-type", "application/json".parse()?);
    headers.insert("authorization", format!("Bearer {api_key}").parse()?);
    headers.insert("OpenAI-Beta", "responses=experimental".parse()?);
    headers.insert("originator", "sidekar".parse()?);

    if !account_id.is_empty() {
        headers.insert("chatgpt-account-id", account_id.parse()?);
    }

    if super::is_verbose() {
        eprintln!("\x1b[2m--- API Request ---");
        eprintln!("POST {url}");
        eprintln!("Headers: {headers:?}");
        eprintln!("Body: {}", serde_json::to_string_pretty(&body).unwrap_or_default());
        eprintln!("---\x1b[0m");
    }

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
        if super::is_verbose() {
            eprintln!("\x1b[2m--- API Error {status} ---\n{text}\n---\x1b[0m");
        }
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
) -> Value {
    let mut input: Vec<Value> = Vec::new();

    for msg in messages {
        match msg.role {
            Role::User => {
                // User text messages
                let text = msg.content.iter().filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                }).collect::<Vec<_>>().join("\n");

                if !text.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "user",
                        "content": text,
                    }));
                }

                // Tool results
                for block in &msg.content {
                    if let ContentBlock::ToolResult { tool_use_id, content, .. } = block {
                        input.push(json!({
                            "type": "function_call_output",
                            "call_id": tool_use_id,
                            "output": content,
                        }));
                    }
                }
            }
            Role::Assistant => {
                // Text output
                let text = msg.content.iter().filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                }).collect::<Vec<_>>().join("\n");

                if !text.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": text,
                    }));
                }

                // Tool calls
                for block in &msg.content {
                    if let ContentBlock::ToolCall { id, name, arguments } = block {
                        input.push(json!({
                            "type": "function_call",
                            "id": id,
                            "call_id": id,
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
        .map(|t| json!({
            "type": "function",
            "name": t.name,
            "description": t.description,
            "parameters": t.input_schema,
        }))
        .collect();

    let mut body = json!({
        "model": model,
        "instructions": system_prompt,
        "input": input,
        "stream": true,
        "store": false,
    });

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
    let mut buffer = String::new();

    let mut content_blocks: Vec<ContentBlock> = Vec::new();
    let mut usage = Usage::default();
    let mut model_id = String::new();
    let mut has_tool_calls = false;
    let mut tool_index = 0usize;

    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error reading SSE chunk")?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        if buffer.contains('\r') {
            buffer = buffer.replace("\r\n", "\n");
        }

        while let Some(event_end) = buffer.find("\n\n") {
            let event_text = buffer[..event_end].to_string();
            buffer = buffer[event_end + 2..].to_string();

            let mut event_data_parts: Vec<String> = Vec::new();
            for line in event_text.lines() {
                if let Some(rest) = line.strip_prefix("data: ") {
                    if rest == "[DONE]" {
                        continue;
                    }
                    event_data_parts.push(rest.to_string());
                } else if let Some(rest) = line.strip_prefix("data:") {
                    if rest.trim() == "[DONE]" {
                        continue;
                    }
                    event_data_parts.push(rest.to_string());
                }
            }

            let event_data = event_data_parts.join("\n");
            if event_data.is_empty() {
                continue;
            }

            let data: Value = match serde_json::from_str(&event_data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let event_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match event_type {
                "response.created" => {
                    model_id = data.get("response")
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
                        let id = item.get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let _ = tx.send(StreamEvent::ToolCallStart {
                            index: tool_index,
                            id: id.clone(),
                            name: name.clone(),
                        });
                    }
                }

                "response.output_text.delta" => {
                    if let Some(delta) = data.get("delta").and_then(|v| v.as_str()) {
                        if !delta.is_empty() {
                            let _ = tx.send(StreamEvent::TextDelta { delta: delta.to_string() });
                        }
                    }
                }

                "response.function_call_arguments.delta" => {
                    if let Some(delta) = data.get("delta").and_then(|v| v.as_str()) {
                        let _ = tx.send(StreamEvent::ToolCallDelta {
                            index: tool_index,
                            delta: delta.to_string(),
                        });
                    }
                }

                "response.function_call_arguments.done" => {
                    let call_id = data.get("item_id")
                        .or_else(|| data.get("call_id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let args_str = data.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
                    let arguments: Value = serde_json::from_str(args_str).unwrap_or(json!({}));

                    let _ = tx.send(StreamEvent::ToolCallEnd { index: tool_index });
                    content_blocks.push(ContentBlock::ToolCall {
                        id: call_id,
                        name,
                        arguments,
                    });
                    tool_index += 1;
                }

                "response.output_text.done" => {
                    if let Some(text) = data.get("text").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            content_blocks.push(ContentBlock::Text { text: text.to_string() });
                        }
                    }
                }

                "response.completed" | "response.done" | "response.incomplete" => {
                    if let Some(resp) = data.get("response") {
                        if let Some(u) = resp.get("usage") {
                            usage.input_tokens = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            usage.output_tokens = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        }
                        let m = resp.get("model").and_then(|v| v.as_str()).unwrap_or("");
                        if !m.is_empty() {
                            model_id = m.to_string();
                        }
                    }

                    let stop = if has_tool_calls { StopReason::ToolUse } else { StopReason::Stop };
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
                    let msg = data.get("response")
                        .and_then(|r| r.get("error"))
                        .and_then(|e| e.get("message"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("Codex request failed");
                    let _ = tx.send(StreamEvent::Error { message: msg.to_string() });
                }

                "error" => {
                    let msg = data.get("message").and_then(|v| v.as_str()).unwrap_or("Unknown Codex error");
                    let _ = tx.send(StreamEvent::Error { message: msg.to_string() });
                }

                _ => {} // Ignore other event types (ping, etc.)
            }
        }
    }

    Ok(())
}
