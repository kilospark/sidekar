//! Two-phase context compaction (hermes-inspired).
//!
//! Phase 1 (cheap): Clear old tool results with "[Cleared]".
//! Phase 2 (LLM):   Summarize middle turns with structured template.

use crate::providers::{ChatMessage, ContentBlock, Provider, Role, StreamEvent};

use super::StreamCallback;

/// Rough token estimate: ~4 chars per token.
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
                    ContentBlock::EncryptedReasoning { encrypted_content, .. } => {
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
    let threshold = (context_window as usize) * 9 / 10;
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

/// Phase 1: Replace old tool result markers and thinking blocks with "[Cleared]".
/// Keeps the last `keep_recent` messages intact.
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
    // Protect first 3 messages and last ~20K tokens worth of messages
    let protect_head = 3.min(history.len());
    let protect_tail_tokens = 20_000;

    // Find the split point from the tail
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

    on_event(&StreamEvent::Connecting);
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
                on_event(&event);
                summary_text.push_str(delta);
            }
            StreamEvent::ThinkingDelta { .. } => {
                on_event(&event);
            }
            StreamEvent::Error { message } => {
                on_event(&event);
                last_error = Some(message.clone());
            }
            StreamEvent::Done { .. } => {
                on_event(&event);
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
                "[CONTEXT COMPACTION] Earlier conversation was summarized:\n\n{summary_text}"
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
