//! Per-request context view builder.
//!
//! Builds a right-sized view of history for each API call without mutating
//! canonical history in ways that break the prompt cache prefix.
//!
//! Two optimizations applied in order:
//! 1. Thinking block eviction — strip from all but the last assistant message.
//! 2. Budget trimming — drop oldest messages if still over token budget.
//!
//! Note: tool-result aging was removed. Distance-based aging mutated a message
//! once it crossed the threshold, destroying the prompt cache key at that
//! position. Context overflow is handled by `compaction::maybe_compact` which
//! fires at ~90% of the context window — a rare event that rebuilds the cache
//! once instead of every few turns.

use std::collections::HashSet;

use crate::providers::{ChatMessage, ContentBlock, Role};

/// Drop `ToolResult` blocks whose `tool_use_id` has no preceding assistant
/// `ToolCall` with the same id. Compaction and budget trimming can leave such
/// orphans behind when they remove an assistant turn but its matching tool
/// result survives in the tail. Anthropic and OpenAI/Codex both reject orphan
/// tool results ("No tool call found for function call output with call_id …").
///
/// Drops user messages that become empty after stripping.
pub(crate) fn drop_orphan_tool_results(messages: &mut Vec<ChatMessage>) {
    let mut seen: HashSet<String> = HashSet::new();
    for msg in messages.iter_mut() {
        match msg.role {
            Role::Assistant => {
                for block in &msg.content {
                    if let ContentBlock::ToolCall { id, .. } = block {
                        seen.insert(id.clone());
                    }
                }
            }
            Role::User => {
                msg.content.retain(|b| match b {
                    ContentBlock::ToolResult { tool_use_id, .. } => seen.contains(tool_use_id),
                    _ => true,
                });
            }
        }
    }
    messages.retain(|m| !m.content.is_empty());
}

/// Rough token estimate: ~4 chars per token (same as compaction).
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
                    } => encrypted_content.len() * 3 / 4,
                })
                .sum::<usize>()
        })
        .sum::<usize>()
        / 4
}

/// Build an ephemeral view of history with thinking eviction and optional
/// budget trimming. Canonical history is not mutated.
pub fn prepare_context(history: &[ChatMessage], token_budget: usize) -> Vec<ChatMessage> {
    // --- Step 0: Drop orphan tool results (compaction/trimming can leave them) ---
    let mut view: Vec<ChatMessage> = history.to_vec();
    drop_orphan_tool_results(&mut view);

    // --- Step 1: Thinking block eviction (ephemeral, view-only) ---

    let last_assistant_idx = view
        .iter()
        .rposition(|m| m.role == Role::Assistant)
        .unwrap_or(usize::MAX);

    for (i, msg) in view.iter_mut().enumerate() {
        if i != last_assistant_idx {
            msg.content
                .retain(|b| !matches!(b, ContentBlock::Thinking { .. }));
        }
    }
    // Drop messages that became empty after stripping.
    view.retain(|m| !m.content.is_empty());

    // --- Step 2: Budget trimming (ephemeral, view-only) ---
    let est = estimate_tokens(&view);
    if est > token_budget && view.len() > 2 {
        // Protect the first message (may contain session context) and the last 5.
        let protect_tail = 5.min(view.len());
        let drop_to = view.len().saturating_sub(protect_tail);
        let mut drop_from = 1; // skip first message
        let mut saved = 0usize;

        while drop_from < drop_to && est.saturating_sub(saved) > token_budget {
            saved += estimate_tokens(std::slice::from_ref(&view[drop_from]));
            drop_from += 1;
        }

        if drop_from > 1 {
            let dropped = drop_from - 1;
            let mut trimmed = Vec::with_capacity(view.len() - dropped + 1);
            trimmed.push(view[0].clone());
            trimmed.push(ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: format!("[{dropped} earlier messages removed to fit context budget]"),
                }],
            });
            trimmed.extend(view[drop_from..].iter().cloned());
            view = trimmed;
            drop_orphan_tool_results(&mut view);
        }
    }

    view
}

#[cfg(test)]
mod tests;
