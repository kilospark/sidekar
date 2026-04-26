//! Two-phase context compaction (hermes-inspired).
//!
//! Phase 1 (cheap): Clear old tool results with "[Cleared]".
//! Phase 2 (LLM):   Summarize middle turns with structured template.

use crate::providers::{ChatMessage, ContentBlock, Provider, Role, StreamEvent};

use super::StreamCallback;

/// Rough token estimate: ~4 chars per token.
///
/// Exposed via `estimate_tokens_public` so `/stats` can show the
/// current context size without duplicating the weighting rules.
pub(crate) fn estimate_tokens_public(messages: &[ChatMessage]) -> usize {
    estimate_tokens(messages)
}

fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|m| {
            m.content
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } => text.len(),
                    ContentBlock::Thinking { thinking, .. } => thinking.len(),
                    ContentBlock::ToolCall { arguments, .. } => arguments.to_string().len(),
                    ContentBlock::ToolResult { content, .. } => content.len(),
                    ContentBlock::Image { data_base64, .. } => data_base64.len(),
                    ContentBlock::EncryptedReasoning {
                        encrypted_content, ..
                    } => {
                        encrypted_content.len() * 3 / 4 // base64 → ~75% raw bytes
                    }
                })
                .sum::<usize>()
        })
        .sum::<usize>()
        / 4
}

/// Check if compaction is needed and perform it if so.
///
/// Returns true if compaction was performed.
pub async fn maybe_compact(
    provider: &Provider,
    model: &str,
    history: &mut Vec<ChatMessage>,
    context_window: u32,
    on_event: &StreamCallback,
) -> bool {
    let threshold = (context_window as usize) * 65 / 100;
    let current = estimate_tokens(history);

    if current < threshold {
        return false;
    }

    compact_history(provider, model, history, on_event, current, true)
        .await
        .unwrap_or(false)
}

/// Force compaction immediately, regardless of the current token estimate.
///
/// Returns true if the history changed.
pub async fn compact_now(
    provider: &Provider,
    model: &str,
    history: &mut Vec<ChatMessage>,
    on_event: &StreamCallback,
) -> bool {
    let current = estimate_tokens(history);
    compact_history(provider, model, history, on_event, current, false)
        .await
        .unwrap_or(false)
}

async fn compact_history(
    provider: &Provider,
    model: &str,
    history: &mut Vec<ChatMessage>,
    on_event: &StreamCallback,
    current: usize,
    stop_after_phase1_if_small: bool,
) -> anyhow::Result<bool> {
    let threshold = current;
    let original_len = history.len();

    // Phase 1: cheap clear of old tool results
    let cleared = phase1_clear_old_results(history);
    if cleared > 0 {
        let after = estimate_tokens(history);
        crate::tunnel::tunnel_println(&format!(
            "\x1b[2m[Compaction phase 1: cleared {} old results, ~{}k → ~{}k tokens]\x1b[0m",
            cleared,
            current / 1000,
            after / 1000,
        ));
        if stop_after_phase1_if_small && after < threshold {
            return Ok(true);
        }
    }

    // Phase 2: LLM summarization
    crate::tunnel::tunnel_println("\x1b[2m[Compaction phase 2: summarizing old context...]\x1b[0m");
    on_event(&StreamEvent::Compacting);
    let result = phase2_summarize(provider, model, history, on_event).await;
    on_event(&StreamEvent::Idle);
    match result {
        Ok(()) => {
            let after = estimate_tokens(history);
            crate::tunnel::tunnel_println(&format!(
                "\x1b[2m[Compacted to ~{}k tokens]\x1b[0m",
                after / 1000
            ));
            Ok(cleared > 0 || history.len() != original_len)
        }
        Err(e) => {
            crate::tunnel::tunnel_println(&format!("\x1b[2m[Compaction failed: {e}]\x1b[0m"));
            if cleared > 0 { Ok(true) } else { Err(e) }
        }
    }
}

/// Phase 1: Replace old tool results, tool call arguments, and thinking
/// blocks with "[Cleared]". Keeps the last `keep_recent` messages intact.
fn phase1_clear_old_results(history: &mut [ChatMessage]) -> usize {
    let keep_recent = 6;
    let cutoff = history.len().saturating_sub(keep_recent);
    let mut cleared = 0;

    for msg in history[..cutoff].iter_mut() {
        for block in msg.content.iter_mut() {
            match block {
                ContentBlock::ToolResult { content, .. } if content.len() > 200 => {
                    *content = "[Cleared]".to_string();
                    cleared += 1;
                }
                ContentBlock::ToolCall { arguments, .. }
                    if arguments.to_string().len() > 200 =>
                {
                    *arguments = serde_json::Value::Object(serde_json::Map::new());
                    cleared += 1;
                }
                ContentBlock::Thinking { thinking, .. } if thinking.len() > 200 => {
                    *thinking = "[Cleared]".to_string();
                    cleared += 1;
                }
                _ => {}
            }
        }
    }

    cleared
}

/// Phase 2: Summarize old messages using the LLM, replace them with a summary.
async fn phase2_summarize(
    provider: &Provider,
    model: &str,
    history: &mut Vec<ChatMessage>,
    on_event: &StreamCallback,
) -> anyhow::Result<()> {
    // Protect first 3 messages and last ~20K tokens worth of messages.
    //
    // In addition, ALWAYS protect the most recent assistant turn and the
    // user message immediately preceding it, verbatim, regardless of token
    // budget. That's where open questions / pending choices live; summarizing
    // them into prose loses the "awaiting user response" signal and the
    // model drifts off-topic after compaction.
    let protect_head = 3.min(history.len());
    let protect_tail_tokens = 20_000;

    // Find the split point from the tail by token budget.
    let mut tail_tokens = 0;
    let mut split = history.len();
    for (i, msg) in history.iter().enumerate().rev() {
        let msg_tokens = estimate_tokens(std::slice::from_ref(msg));
        tail_tokens += msg_tokens;
        if tail_tokens > protect_tail_tokens {
            split = i + 1;
            break;
        }
    }

    // Force-protect the last assistant message and its preceding user message.
    // Walk back from the end; find the last Assistant index, then the nearest
    // User index before it. Clamp `split` so both are in the protected tail.
    if let Some(last_assistant_idx) = history
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, m)| (m.role == Role::Assistant).then_some(i))
    {
        let pair_start = history[..last_assistant_idx]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, m)| (m.role == Role::User).then_some(i))
            .unwrap_or(last_assistant_idx);
        if pair_start < split {
            split = pair_start;
        }
    }

    split = split.max(protect_head);

    if split <= protect_head {
        // Nothing to summarize
        return Ok(());
    }

    let to_summarize = &history[protect_head..split];
    if to_summarize.is_empty() {
        return Ok(());
    }

    // Build summarization prompt
    let mut summary_input = String::new();
    for msg in to_summarize {
        let role = match msg.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
        };
        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => {
                    summary_input.push_str(&format!("{role}: {}\n", truncate(text, 2000)));
                }
                ContentBlock::ToolCall { name, .. } => {
                    summary_input.push_str(&format!("{role}: [called tool: {name}]\n"));
                }
                ContentBlock::ToolResult {
                    content, is_error, ..
                } => {
                    let prefix = if *is_error { "ERROR" } else { "result" };
                    summary_input.push_str(&format!(
                        "{role}: [tool {prefix}]: {}\n",
                        truncate(content, 500)
                    ));
                }
                ContentBlock::Thinking { .. } => {}
                ContentBlock::Image { .. } => {
                    summary_input.push_str(&format!("{role}: [image attachment]\n"));
                }
                ContentBlock::EncryptedReasoning { .. } => {
                    // Opaque blob — nothing useful to summarize.
                }
            }
        }
    }

    let summary_prompt = format!(
        "Summarize the following conversation turns into a structured context summary. \
        Be specific — include file paths, decisions, errors encountered, and current state.\n\n\
        Use this format:\n\
        ## Goal\n[User's objective]\n\n\
        ## Progress\n### Done\n[Completed work]\n### In Progress\n[Current work]\n\n\
        ## Key Decisions\n[Technical decisions made]\n\n\
        ## Relevant Files\n[Files read/modified]\n\n\
        ## Next Steps\n[What must happen next]\n\n\
        ## Critical Context\n[Values, errors, config details]\n\n\
        ## Awaiting User Response\n\
        [If the assistant's LAST message in this range posed a question, \
        offered options, or requested a decision from the user, quote it \
        verbatim here and label it \"OPEN QUESTION — do not answer until \
        the user addresses it\". If the user's next message pivots to a \
        different topic, the open question remains parked — acknowledge \
        the pivot explicitly rather than silently dropping it. If no open \
        question exists, write \"None\".]\n\n\
        ---\n\
        Conversation to summarize:\n\n{summary_input}"
    );

    // Call the LLM for summarization (no tools, single turn)
    let summary_messages = vec![ChatMessage {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: summary_prompt,
        }],
    }];

    // We deliberately do NOT forward the summarizer's content
    // events (TextDelta, ThinkingDelta, or the Done message's
    // content) to `on_event`. The summary is a PROMPT ARTIFACT —
    // it replaces a chunk of history behind a `[CONTEXT
    // COMPACTION]` marker and is consumed by the model on the
    // next turn. Showing it to the user makes the terminal
    // spit out a multi-kilobyte structured recap every time
    // compaction fires (both /compact and the silent
    // auto-compaction path), which is exactly the opposite of
    // what compaction is supposed to feel like.
    //
    // Still forward status/lifecycle events so the renderer's
    // "[Compacting]" indicator works: the outer compact_history
    // fn already fires Compacting / Idle. Error events pass
    // through so failures are visible; the inner content stays
    // silent.
    //
    // We also don't fire Connecting here for the same reason —
    // the user already saw "[Compaction phase 2: summarizing
    // old context...]" from compact_history. A second
    // Connecting indicator would race with the first.
    let (mut rx, _reclaim) = provider
        .stream(
            model,
            "You are a precise conversation summarizer. Output only the structured summary.",
            &summary_messages,
            &[],
            None,
            None,
            None,
        )
        .await?;

    let mut summary_text = String::new();
    let mut last_error: Option<String> = None;
    while let Some(event) = rx.recv().await {
        match &event {
            StreamEvent::TextDelta { delta } => {
                // Accumulate silently; do NOT render.
                summary_text.push_str(delta);
            }
            StreamEvent::ThinkingDelta { .. } => {
                // Silent.
            }
            StreamEvent::Error { message } => {
                // Errors are the only summarizer event we want
                // user-visible — compaction failure should not
                // be silent, it's a real signal (auth, rate
                // limit, transport).
                on_event(&event);
                last_error = Some(message.clone());
            }
            StreamEvent::Done { .. } => {
                // Silent. The outer compact_history fn prints
                // "[Compacted to ~Nk tokens]" which is the
                // user-visible completion marker.
            }
            _ => {}
        }
    }

    if let Some(err) = last_error {
        anyhow::bail!("LLM stream error: {err}");
    }

    if summary_text.is_empty() {
        anyhow::bail!("Empty summary from LLM");
    }

    // Replace old messages with summary
    let head: Vec<ChatMessage> = history[..protect_head].to_vec();
    let tail: Vec<ChatMessage> = history[split..].to_vec();

    history.clear();
    history.extend(head);
    history.push(ChatMessage {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: format!(
                "[CONTEXT COMPACTION] Earlier conversation was summarized below. \
                The messages after this marker are preserved verbatim — treat the \
                most recent assistant message as your immediate prior turn. If the \
                summary's \"Awaiting User Response\" section names an open question \
                and the user's next message does not address it, acknowledge the \
                pivot explicitly before proceeding (do not silently abandon the \
                open thread).\n\n{summary_text}"
            ),
        }],
    });
    history.extend(tail);

    Ok(())
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..s.floor_char_boundary(max)]
    }
}
