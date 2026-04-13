pub mod compaction;
pub mod context;
pub mod images;
pub mod tools;

use anyhow::{Result, bail};
use tokio::sync::mpsc;

use crate::providers::{
    AssistantResponse, ChatMessage, ContentBlock, Provider, Role, StopReason, StreamEvent, ToolDef,
    codex,
};

static ERROR_DISPLAYED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_error_displayed(b: bool) {
    ERROR_DISPLAYED.store(b, std::sync::atomic::Ordering::SeqCst);
}

pub fn take_error_displayed() -> bool {
    ERROR_DISPLAYED.swap(false, std::sync::atomic::Ordering::SeqCst)
}

/// Callback for streaming events to the REPL.
pub type StreamCallback = Box<dyn Fn(&StreamEvent) + Send>;

/// Returned when the user cancels via Escape.
#[derive(Debug)]
pub struct Cancelled;

/// Run the agent loop: stream LLM response, execute tool calls, repeat.
/// Returns `Ok(true)` if history was compacted during the loop.
///
/// `previous_response_id` enables stateful chaining for providers that
/// support it (codex). On entry it may contain the response ID from a prior
/// `run()` call. On exit it is updated to the ID of the last successful
/// response so the caller can pass it into the next `run()`. Compaction
/// resets it to `None` because the server-side history is no longer valid.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    provider: &Provider,
    model: &str,
    system_prompt: &str,
    history: &mut Vec<ChatMessage>,
    tool_defs: &[ToolDef],
    on_event: StreamCallback,
    cancel: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
    prompt_cache_key: Option<&str>,
    previous_response_id: &mut Option<String>,
    cached_ws: &mut Option<codex::CachedWs>,
) -> Result<bool, anyhow::Error> {
    // Reset error flag from any prior turn so a stale flag doesn't suppress
    // error display in this turn.
    set_error_displayed(false);

    let mut context_window: Option<u32> = None;
    let mut did_compact = false;
    let mut in_tool_loop = false;

    loop {
        if let Some(c) = cancel
            && c.load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(Cancelled.into());
        }

        // On the first iteration, show status spinners. On subsequent
        // iterations (tool-call chains), skip them so the ToolExec spinner
        // ("running Sidekar — ...") stays visible until the next response
        // starts streaming.
        if !in_tool_loop {
            on_event(&StreamEvent::Waiting);
        }

        let context_window = match context_window {
            Some(v) => v,
            None => {
                on_event(&StreamEvent::ResolvingContext);
                let v = crate::providers::fetch_context_window(model, provider).await;
                context_window = Some(v);
                v
            }
        };

        // Auto-compact if context is getting large. Compaction rewrites
        // history so the server-side chain is no longer valid.
        let compacted =
            compaction::maybe_compact(provider, model, history, context_window, &on_event).await;
        if compacted {
            did_compact = true;
            *previous_response_id = None;
        }

        if !in_tool_loop {
            on_event(&StreamEvent::Waiting);
            on_event(&StreamEvent::Connecting);
        }

        let prev_id_ref = previous_response_id.as_deref();
        let system_tokens = system_prompt.len() / 4;
        let tool_tokens: usize = tool_defs
            .iter()
            .map(|t| t.name.len() + t.description.len() + t.input_schema.to_string().len())
            .sum::<usize>()
            / 4;
        let response_reserve = 16_000;
        let history_budget = (context_window as usize)
            .saturating_sub(system_tokens)
            .saturating_sub(tool_tokens)
            .saturating_sub(response_reserve);
        let view = context::prepare_context(history, history_budget);

        if !in_tool_loop {
            on_event(&StreamEvent::Connecting);
        }
        // Stream LLM response — select against cancel so Esc works during connection.
        let ws = cached_ws.take();
        let stream_result = match cancel {
            Some(c) => {
                tokio::select! {
                    _ = wait_for_cancel(c) => return Err(Cancelled.into()),
                    result = provider.stream(model, system_prompt, &view, tool_defs, prompt_cache_key, prev_id_ref, ws) => result,
                }
            }
            None => {
                provider
                    .stream(
                        model,
                        system_prompt,
                        &view,
                        tool_defs,
                        prompt_cache_key,
                        prev_id_ref,
                        ws,
                    )
                    .await
            }
        };
        let (mut rx, reclaim_rx) = match stream_result {
            Ok(pair) => pair,
            Err(e) => {
                on_event(&StreamEvent::Error {
                    message: format!("{e:#}"),
                });
                set_error_displayed(true);
                return Err(e);
            }
        };

        // Stream opened — transition from "connecting" to "waiting for
        // response" until the first delta arrives.
        on_event(&StreamEvent::Waiting);

        let response = match consume_stream(&mut rx, &on_event, cancel).await {
            Ok(r) => r,
            Err(e) if e.is::<Cancelled>() => return Err(e),
            Err(e) => return Err(e),
        };

        // Reclaim WS connection for reuse on the next turn
        *cached_ws = reclaim_rx.await.unwrap_or(None);
        if crate::providers::is_verbose() {
            crate::tunnel::tunnel_println(&format!(
                "\x1b[2m[ws] reclaim result: {}\x1b[0m",
                if cached_ws.is_some() {
                    "got connection"
                } else {
                    "none"
                }
            ));
        }

        // Update stateful chaining state.
        if !response.response_id.is_empty() {
            *previous_response_id = Some(response.response_id.clone());
        }

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
            if let Some(c) = cancel
                && c.load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(Cancelled.into());
            }
            let arguments_json =
                serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_string());
            on_event(&StreamEvent::ToolExec {
                name: name.clone(),
                arguments_json,
            });
            let result = tools::execute(name, arguments, cancel).await;
            let (content, is_error) = match result {
                Ok(output) => (truncate_tool_output(&output, 50_000), false),
                Err(e) if e.is::<Cancelled>() => return Err(e),
                Err(e) => {
                    crate::broker::try_log_error(
                        "repl",
                        &format!("tool {name} failed"),
                        Some(&format!("{e:#}")),
                    );
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

        in_tool_loop = true;
    }

    Ok(did_compact)
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
        if let Some(c) = cancel
            && c.load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(Cancelled.into());
        }

        let event = match cancel {
            Some(c) => {
                tokio::select! {
                    _ = wait_for_cancel(c) => return Err(Cancelled.into()),
                    event = rx.recv() => event,
                }
            }
            None => rx.recv().await,
        };
        let event = match event {
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
                set_error_displayed(true);
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

async fn wait_for_cancel(cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>) {
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
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
