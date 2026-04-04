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

/// Returned when the user cancels via Escape.
#[derive(Debug)]
pub struct Cancelled;

/// Run the agent loop: stream LLM response, execute tool calls, repeat.
/// If `cancel` is provided and set to true, the loop aborts early.
pub async fn run(
    provider: &Provider,
    model: &str,
    system_prompt: &str,
    history: &mut Vec<ChatMessage>,
    tool_defs: &[ToolDef],
    on_event: StreamCallback,
    cancel: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<(), anyhow::Error> {
    let mut iteration = 0;

    let context_window = crate::providers::fetch_context_window(model, provider).await;

    loop {
        if let Some(c) = cancel {
            if c.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(Cancelled.into());
            }
        }

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

        // Signal UI to show waiting indicator
        on_event(&StreamEvent::Waiting);

        // Stream LLM response
        let mut rx = provider
            .stream(model, system_prompt, history, tool_defs)
            .await?;

        let response = match consume_stream(&mut rx, &on_event, cancel).await {
            Ok(r) => r,
            Err(e) if e.is::<Cancelled>() => return Err(e),
            Err(e) => return Err(e),
        };

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
            on_event(&StreamEvent::ToolExec { name: name.clone() });
            let result = tools::execute(name, arguments).await;
            let (content, is_error) = match result {
                Ok(output) => (truncate_tool_output(&output, 50_000), false),
                Err(e) => {
                    crate::broker::try_log_error("repl", &format!("tool {name} failed"), Some(&format!("{e:#}")));
                    (format!("Error: {e:#}"), true)
                }
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

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cancelled")
    }
}
impl std::error::Error for Cancelled {}

/// Consume all events from the stream, forwarding to the callback.
async fn consume_stream(
    rx: &mut mpsc::UnboundedReceiver<StreamEvent>,
    on_event: &StreamCallback,
    cancel: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<AssistantResponse> {
    let mut final_response: Option<AssistantResponse> = None;
    let mut last_error: Option<String> = None;

    loop {
        // Check cancel flag between events
        if let Some(c) = cancel {
            if c.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(Cancelled.into());
            }
        }

        let event = match rx.recv().await {
            Some(e) => e,
            None => break,
        };

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
        crate::broker::try_log_error("repl", "LLM stream error", Some(&err));
        bail!("LLM stream error: {}", err)
    } else {
        crate::broker::try_log_error("repl", "LLM stream ended without a response", None);
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
