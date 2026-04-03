pub mod compaction;
pub mod tools;

use anyhow::{Result, bail};
use tokio::sync::mpsc;

use crate::providers::{
    AssistantResponse, ChatMessage, ContentBlock, Provider, Role, StopReason, StreamEvent, ToolDef,
};

const MAX_ITERATIONS: usize = 25;

/// Callback for streaming events to the REPL.
pub type StreamCallback = Box<dyn Fn(&StreamEvent) + Send>;

/// Run the agent loop: stream LLM response, execute tool calls, repeat.
pub async fn run(
    provider: &Provider,
    model: &str,
    system_prompt: &str,
    history: &mut Vec<ChatMessage>,
    tool_defs: &[ToolDef],
    on_event: StreamCallback,
) -> Result<()> {
    let mut iteration = 0;

    let context_window = crate::providers::model_info(model)
        .map(|m| m.context_window)
        .unwrap_or(200_000);

    loop {
        if iteration >= MAX_ITERATIONS {
            eprintln!(
                "\nsidekar: reached max iterations ({}), stopping",
                MAX_ITERATIONS
            );
            break;
        }
        iteration += 1;

        // Auto-compact if context is getting large
        compaction::maybe_compact(provider, model, history, context_window).await;

        // Stream LLM response
        let mut rx = provider
            .stream(model, system_prompt, history, tool_defs)
            .await?;

        let response = consume_stream(&mut rx, &on_event).await?;

        // Add assistant message to history
        history.push(ChatMessage {
            role: Role::Assistant,
            content: response.content.clone(),
        });

        // Extract tool calls
        let tool_calls: Vec<_> = response
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                } => Some((id.clone(), name.clone(), arguments.clone())),
                _ => None,
            })
            .collect();

        if tool_calls.is_empty() || response.stop_reason != StopReason::ToolUse {
            break;
        }

        // Execute tool calls, build tool_result content blocks
        let mut result_blocks: Vec<ContentBlock> = Vec::new();
        for (id, name, arguments) in &tool_calls {
            let result = tools::execute(name, arguments).await;
            let (content, is_error) = match result {
                Ok(output) => (truncate_tool_output(&output, 50_000), false),
                Err(e) => (format!("Error: {e:#}"), true),
            };
            result_blocks.push(ContentBlock::ToolResult {
                tool_use_id: id.clone(),
                content,
                is_error,
            });
        }

        // Add tool results as a user message (Anthropic API format)
        history.push(ChatMessage {
            role: Role::User,
            content: result_blocks,
        });
    }

    Ok(())
}

/// Consume all events from the stream, forwarding to the callback.
async fn consume_stream(
    rx: &mut mpsc::UnboundedReceiver<StreamEvent>,
    on_event: &StreamCallback,
) -> Result<AssistantResponse> {
    let mut final_response: Option<AssistantResponse> = None;
    let mut last_error: Option<String> = None;

    while let Some(event) = rx.recv().await {
        match &event {
            StreamEvent::Done { message } => {
                on_event(&event);
                final_response = Some(message.clone());
            }
            StreamEvent::Error { message } => {
                on_event(&event);
                last_error = Some(message.clone());
            }
            _ => {
                on_event(&event);
            }
        }
    }

    if let Some(response) = final_response {
        Ok(response)
    } else if let Some(err) = last_error {
        bail!("LLM stream error: {}", err)
    } else {
        bail!("LLM stream ended without a response")
    }
}

/// Truncate tool output, respecting UTF-8 char boundaries.
fn truncate_tool_output(output: &str, max_bytes: usize) -> String {
    if output.len() <= max_bytes {
        return output.to_string();
    }
    let half = max_bytes / 2;
    // Find safe char boundaries
    let head_end = output.floor_char_boundary(half);
    let tail_start = output.ceil_char_boundary(output.len() - half);
    let head = &output[..head_end];
    let tail = &output[tail_start..];
    format!(
        "{}\n\n[... truncated {} bytes ...]\n\n{}",
        head,
        output.len() - max_bytes,
        tail
    )
}
