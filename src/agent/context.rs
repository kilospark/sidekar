//! Per-request context view builder.
//!
//! Builds a right-sized view of history for each API call.
//!
//! Tool-result aging is applied **in-place** on canonical history so that once a
//! result is truncated it stays byte-identical on subsequent turns — preserving
//! the prompt cache prefix.  Thinking eviction and budget trimming remain
//! ephemeral (view-only).
//!
//! Three optimizations applied in order:
//! 1. Progressive tool-result aging — **in-place** on canonical history.
//! 2. Thinking block eviction — strip from all but the last assistant message.
//! 3. Budget trimming — drop oldest messages if still over token budget.

use crate::providers::{ChatMessage, ContentBlock, Role};

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
                })
                .sum::<usize>()
        })
        .sum::<usize>()
        / 4
}

/// Age tool results in-place on canonical history, then build an ephemeral view
/// with thinking eviction and budget trimming.
pub fn prepare_context(history: &mut Vec<ChatMessage>, token_budget: usize) -> Vec<ChatMessage> {
    // --- Step 1: Progressive tool-result aging (in-place on canonical history) ---
    // Applied directly so that once a result is aged, it stays stable across
    // subsequent turns — the prompt prefix never changes due to aging.
    // Skip results already aged (prefixed with "[Aged]" or "[" stub marker) to
    // guarantee byte-stability.
    let len = history.len();
    for (i, msg) in history.iter_mut().enumerate() {
        let distance = len.saturating_sub(1).saturating_sub(i);
        for block in msg.content.iter_mut() {
            if let ContentBlock::ToolResult { content, .. } = block {
                if content.starts_with("[Aged]") || content.starts_with("[Cleared]") {
                    continue; // already aged — don't re-age
                }
                if distance >= 15 && content.len() > 200 {
                    *content = stub_tool_result(content);
                } else if distance >= 5 && content.len() > 2048 {
                    let truncated = truncate(content, 2048).to_string();
                    *content = format!("[Aged] {truncated}");
                }
            }
        }
    }

    // --- Step 2: Thinking block eviction (ephemeral, view-only) ---
    let mut view: Vec<ChatMessage> = history.to_vec();

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

    // --- Step 3: Budget trimming (ephemeral, view-only) ---
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
        }
    }

    view
}

/// Compress a tool result down to a one-line stub with byte count.
fn stub_tool_result(content: &str) -> String {
    let first_line = content
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("(empty)");
    let truncated = truncate(first_line, 120);
    format!("[{} bytes] {}", content.len(), truncated)
}

/// Truncate a string at a safe UTF-8 char boundary.
fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..s.floor_char_boundary(max)]
    }
}

#[cfg(test)]
mod tests;
