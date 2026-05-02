pub mod compaction;
pub mod context;
pub(crate) mod edit_patch;
pub mod images;
pub mod tools;
#[cfg(unix)]
pub mod unified_exec;

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

/// Mid-stream error before any content was emitted to the caller.
/// Signals that the stream opened successfully, got no TextDelta /
/// ToolCallStart / Thinking, then failed with a retryable error.
/// Safe to retry the whole turn because nothing has been rendered
/// to the user and no partial history entry exists.
///
/// The inner `String` is the underlying error message — the caller
/// (`run`) passes it through `is_retryable_error` to decide whether
/// to actually retry.
///
/// Not retryable in this sense:
///   - Errors AFTER content started flowing. The user has already
///     seen partial output; re-streaming would double-render.
///   - Auth errors (401/403). Handled by the existing auth-refresh
///     branch in `Provider::stream`.
///   - Non-retryable classes (4xx except 429, malformed response).
///     Bubble up unchanged.
#[derive(Debug)]
pub struct MidStreamNoContent(pub String);

impl std::fmt::Display for MidStreamNoContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "mid-stream failure before any content: {}", self.0)
    }
}
impl std::error::Error for MidStreamNoContent {}

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
                if crate::providers::cached_context_window(model).is_none() {
                    on_event(&StreamEvent::ResolvingContext);
                }
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

        // Mid-stream retry loop. The provider layer (stream_once)
        // already retries transient failures at the HTTP-open
        // boundary — those fire before any StreamEvent::TextDelta /
        // ThinkingDelta / ToolCallStart has crossed the channel.
        //
        // This loop catches the other half of the problem:
        // connection-reset / incomplete-SSE failures that happen
        // AFTER the stream opened but BEFORE any content flowed.
        // `consume_stream` surfaces those as MidStreamNoContent;
        // we retry the whole turn (open + consume) up to
        // STREAM_CONTENT_RETRIES times before giving up.
        //
        // Retry budget mirrors the open-side retry (3 attempts).
        // Backoff is exponential starting at 500ms. Uses the same
        // is_retryable_error classifier as stream_once so the
        // two layers agree on what "retryable" means.
        //
        // If content has already rendered to the user, the stream
        // error propagates unchanged — re-streaming would double-
        // render tokens and corrupt the session transcript.
        const STREAM_CONTENT_RETRIES: u32 = 3;
        let mut stream_attempt = 0u32;
        let response = loop {
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
            let (mut rx, reclaim_rx_local) = match stream_result {
                Ok(pair) => pair,
                Err(e) => {
                    on_event(&StreamEvent::Error {
                        message: format!("{e:#}"),
                    });
                    set_error_displayed(true);
                    return Err(e);
                }
            };

            // Stream opened — transition from "connecting" to
            // "waiting for response" until the first delta arrives.
            on_event(&StreamEvent::Waiting);

            match consume_stream(&mut rx, &on_event, cancel).await {
                Ok(r) => {
                    // Bind the outer reclaim_rx for the hop below.
                    // The compiler's borrow checker forbids us
                    // binding reclaim_rx to a Let outside the
                    // loop because each iteration produces a fresh
                    // one; we instead consume it right here and
                    // stash the result into the outer cached_ws.
                    *cached_ws = reclaim_rx_local.await.unwrap_or(None);
                    break r;
                }
                Err(e) if e.is::<Cancelled>() => return Err(e),
                Err(e) => {
                    // If the error fired before any content
                    // rendered AND is_retryable_error agrees, loop.
                    // Otherwise propagate.
                    let is_empty_midstream = e.is::<MidStreamNoContent>();
                    let should_retry = is_empty_midstream
                        && stream_attempt < STREAM_CONTENT_RETRIES
                        && crate::providers::is_retryable_error(&e);
                    if should_retry {
                        stream_attempt += 1;
                        let delay =
                            std::time::Duration::from_millis(500 * 2u64.pow(stream_attempt - 1));
                        crate::broker::try_log_error(
                            "repl",
                            &format!(
                                "mid-stream retry ({stream_attempt}/{STREAM_CONTENT_RETRIES})"
                            ),
                            Some(&format!("{e:#}")),
                        );
                        crate::broker::try_log_event(
                            "debug",
                            "repl",
                            "mid-stream-retry-scheduled",
                            Some(&format!(
                                "attempt={stream_attempt} max={} delay_secs={:.1}",
                                STREAM_CONTENT_RETRIES,
                                delay.as_secs_f32()
                            )),
                        );
                        // Discard the failed reclaim handle —
                        // there's no live WS to reclaim, and
                        // stream_once will open a fresh
                        // connection next iteration. Dropping
                        // here is explicit and intentional.
                        drop(reclaim_rx_local);
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(e);
                }
            }
        };

        // WS reclaim already happened inside the mid-stream retry
        // loop above on the success path (the `Ok(r) => {}` arm
        // writes into *cached_ws before break'ing out). Keeping
        // the log line here for the success case so the "[ws]
        // reclaim result" trace is preserved.
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
                    ..
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
                Err(e) if e.is::<Cancelled>() => {
                    // Give the user explicit feedback that their Esc/Ctrl+C
                    // landed and the tool tree was killed — otherwise the
                    // turn unwinds silently and it looks like nothing
                    // happened even though cancel did propagate.
                    crate::tunnel::tunnel_println(&format!("\x1b[33m[cancelled: {name}]\x1b[0m"));
                    return Err(e);
                }
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
    // Track whether the model has emitted any user-visible content
    // yet. A retry after content has flowed would double-render,
    // so mid-stream failures past this point cannot be transparently
    // retried. Events that count as content: TextDelta, Thinking,
    // ToolCallStart. Connecting / Waiting / status events do not.
    let mut emitted_content = false;

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
            // Content events — after one of these we can't safely
            // retry without re-emitting what the user already saw.
            // Matches TextDelta (user-visible tokens), ThinkingDelta
            // (extended thinking tokens, also rendered), and
            // ToolCallStart (the first name/args bytes of a tool).
            StreamEvent::TextDelta { .. }
            | StreamEvent::ThinkingDelta { .. }
            | StreamEvent::ToolCallStart { .. } => {
                emitted_content = true;
                on_event(&event);
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
        // Before any content has been emitted, surface a typed
        // error so the caller can retry the turn transparently.
        // Past the first content event, retrying would double-
        // render — propagate unchanged.
        if !emitted_content {
            return Err(MidStreamNoContent(err).into());
        }
        bail!("LLM stream error: {}", err)
    } else {
        crate::broker::try_log_error("repl", "LLM stream ended without a response", None);
        // Zero events arrived and no explicit error. Treat as a
        // retryable mid-stream failure (empty stream is almost
        // always a proxy or connection-reset edge case).
        if !emitted_content {
            return Err(MidStreamNoContent("stream ended without a response".to_string()).into());
        }
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

#[cfg(test)]
mod consume_stream_tests {
    //! Unit tests for the mid-stream retry boundary.
    //!
    //! `consume_stream` is the split point that decides whether a
    //! turn can be safely retried: returning `MidStreamNoContent`
    //! on an error that preceded any user-visible event; a plain
    //! `anyhow::Error` otherwise (once tokens have rendered,
    //! re-streaming would double-render).
    //!
    //! The retry wrapper itself lives inside `run`, which is hard
    //! to unit-test (Provider + agent loop). These tests cover the
    //! classifier that drives it, leaving the loop's control flow
    //! to integration runs. Classifier correctness is the hard
    //! part — a regression that emits MidStreamNoContent after
    //! content has rendered would cause double-rendering in the
    //! live REPL.

    use super::*;
    use crate::providers::AssistantResponse;
    use tokio::sync::mpsc;

    fn noop_callback() -> StreamCallback {
        Box::new(|_: &StreamEvent| {})
    }

    async fn consume(events: Vec<StreamEvent>) -> Result<AssistantResponse> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        for e in events {
            tx.send(e).unwrap();
        }
        drop(tx);
        let cb = noop_callback();
        consume_stream(&mut rx, &cb, None).await
    }

    fn empty_response() -> AssistantResponse {
        AssistantResponse {
            content: vec![],
            stop_reason: StopReason::Stop,
            response_id: String::new(),
            model: String::new(),
            usage: Default::default(),
            rate_limit: None,
        }
    }

    #[tokio::test]
    async fn error_before_content_returns_midstream_no_content() {
        // Simulated flow: stream opened, no TextDelta arrived, the
        // provider emitted an Error event (the SSE chunk read
        // failure path). Must surface MidStreamNoContent so the
        // retry wrapper can act.
        let err = consume(vec![
            StreamEvent::Waiting,
            StreamEvent::Error {
                message: "connection reset by peer".to_string(),
            },
        ])
        .await
        .unwrap_err();
        assert!(
            err.is::<MidStreamNoContent>(),
            "expected MidStreamNoContent, got: {err:#}"
        );
    }

    #[tokio::test]
    async fn error_after_text_delta_does_not_return_midstream_no_content() {
        // Classifier invariant: once any TextDelta has crossed,
        // the error path must NOT yield MidStreamNoContent —
        // double-rendering protection.
        let err = consume(vec![
            StreamEvent::TextDelta {
                delta: "partial ".to_string(),
            },
            StreamEvent::Error {
                message: "connection reset by peer".to_string(),
            },
        ])
        .await
        .unwrap_err();
        assert!(
            !err.is::<MidStreamNoContent>(),
            "expected plain error after content, got MidStreamNoContent"
        );
        // Original message still visible in the error chain.
        let s = format!("{err:#}");
        assert!(s.contains("connection reset by peer"), "got: {s}");
    }

    #[tokio::test]
    async fn error_after_thinking_delta_does_not_retry() {
        // Thinking tokens are user-visible on models with
        // extended thinking — they render to the terminal. Same
        // double-render rule applies.
        let err = consume(vec![
            StreamEvent::ThinkingDelta {
                delta: "hm, let me ".to_string(),
            },
            StreamEvent::Error {
                message: "eof".to_string(),
            },
        ])
        .await
        .unwrap_err();
        assert!(!err.is::<MidStreamNoContent>(), "got: {err:#}");
    }

    #[tokio::test]
    async fn error_after_tool_call_start_does_not_retry() {
        // ToolCallStart is the first moment a tool panel appears
        // on screen. Retrying would re-show the panel and the
        // model might re-issue different args.
        let err = consume(vec![
            StreamEvent::ToolCallStart {
                index: 0,
                id: "t-1".to_string(),
                name: "Bash".to_string(),
            },
            StreamEvent::Error {
                message: "eof".to_string(),
            },
        ])
        .await
        .unwrap_err();
        assert!(!err.is::<MidStreamNoContent>(), "got: {err:#}");
    }

    #[tokio::test]
    async fn connecting_and_waiting_alone_do_not_count_as_content() {
        // Spec: Connecting and Waiting are status events. A stream
        // that only emits those before failing is still a zero-
        // content failure and must be retryable.
        let err = consume(vec![
            StreamEvent::Connecting,
            StreamEvent::Waiting,
            StreamEvent::Error {
                message: "timed out".to_string(),
            },
        ])
        .await
        .unwrap_err();
        assert!(
            err.is::<MidStreamNoContent>(),
            "status-only prefix should not block retry: {err:#}"
        );
    }

    #[tokio::test]
    async fn channel_closed_without_events_is_midstream_no_content() {
        // Provider task dropped the sender without ever firing
        // Error or Done. This manifests as rx returning None on
        // the first recv(). Should be retryable — it's almost
        // always a transient connection drop at the transport
        // layer that never made it to the SSE parser.
        let err = consume(vec![]).await.unwrap_err();
        assert!(err.is::<MidStreamNoContent>(), "got: {err:#}");
    }

    #[tokio::test]
    async fn done_event_returns_ok_regardless_of_prior_content() {
        // Sanity: the success path is unchanged. Done yields the
        // message, never an error.
        let resp = consume(vec![
            StreamEvent::TextDelta {
                delta: "hello".to_string(),
            },
            StreamEvent::Done {
                message: empty_response(),
            },
        ])
        .await
        .expect("done should produce Ok");
        assert!(matches!(resp.stop_reason, StopReason::Stop));
    }

    #[tokio::test]
    async fn midstream_no_content_message_survives_round_trip() {
        // The inner String of MidStreamNoContent must carry the
        // original provider error so is_retryable_error can
        // classify it. If this round-trip ever breaks, the retry
        // loop would silently become a blanket retry regardless
        // of error class.
        let err = consume(vec![StreamEvent::Error {
            message: "(502) bad gateway".to_string(),
        }])
        .await
        .unwrap_err();
        let inner = err
            .downcast_ref::<MidStreamNoContent>()
            .expect("must be MidStreamNoContent");
        assert_eq!(inner.0, "(502) bad gateway");
    }
}
