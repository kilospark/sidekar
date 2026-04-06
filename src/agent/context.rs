//! Per-request context view builder.
//!
//! Builds an ephemeral, right-sized view of history for each API call without
//! mutating the canonical history (which stays intact for persistence).
//!
//! Three optimizations applied in order:
//! 1. Thinking block eviction — strip from all but the last assistant message.
//! 2. Progressive tool-result aging — shrink old results by distance from tail.
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
                })
                .sum::<usize>()
        })
        .sum::<usize>()
        / 4
}

/// Build an optimised view of `history` that fits within `token_budget`.
///
/// The returned vec is ephemeral — send it to the LLM, then discard it.
/// Canonical history is never modified.
pub fn prepare_context(history: &[ChatMessage], token_budget: usize) -> Vec<ChatMessage> {
    let mut view: Vec<ChatMessage> = history.to_vec();

    // --- Step 1: Thinking block eviction ---
    // Keep thinking only on the last assistant message (the API may use it for
    // extended-thinking continuation). Strip from everything else.
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

    // --- Step 2: Progressive tool-result aging ---
    let len = view.len();
    for (i, msg) in view.iter_mut().enumerate() {
        let distance = len.saturating_sub(1).saturating_sub(i);
        for block in msg.content.iter_mut() {
            if let ContentBlock::ToolResult { content, .. } = block {
                if distance >= 15 && content.len() > 200 {
                    *content = stub_tool_result(content);
                } else if distance >= 5 && content.len() > 2048 {
                    let truncated = truncate(content, 2048);
                    *content = format!("[Aged] {truncated}");
                }
            }
        }
    }

    // --- Step 3: Budget trimming ---
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
                    text: format!(
                        "[{dropped} earlier messages removed to fit context budget]"
                    ),
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
mod tests {
    use super::*;
    use crate::providers::{ChatMessage, ContentBlock, Role};

    fn text_msg(role: Role, text: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    fn thinking_msg(thinking: &str, visible: &str) -> ChatMessage {
        ChatMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: thinking.to_string(),
                    signature: "sig".to_string(),
                },
                ContentBlock::Text {
                    text: visible.to_string(),
                },
            ],
        }
    }

    fn tool_result_msg(id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: content.to_string(),
                is_error: false,
            }],
        }
    }

    #[test]
    fn thinking_evicted_except_last_assistant() {
        let history = vec![
            text_msg(Role::User, "hello"),
            thinking_msg("old reasoning", "response 1"),
            text_msg(Role::User, "next"),
            thinking_msg("recent reasoning", "response 2"),
        ];

        let view = prepare_context(&history, 1_000_000);

        // First assistant: thinking stripped, only text remains
        let first_asst = view.iter().find(|m| {
            m.role == Role::Assistant
                && m.content.iter().any(|b| matches!(b, ContentBlock::Text { text } if text == "response 1"))
        }).expect("first assistant message should exist");
        assert!(
            !first_asst.content.iter().any(|b| matches!(b, ContentBlock::Thinking { .. })),
            "thinking should be stripped from non-last assistant"
        );

        // Last assistant: thinking preserved
        let last_asst = view.last().unwrap();
        assert!(
            last_asst.content.iter().any(|b| matches!(b, ContentBlock::Thinking { .. })),
            "thinking should be preserved on last assistant"
        );
    }

    #[test]
    fn empty_messages_removed_after_eviction() {
        // An assistant message with only a thinking block should be dropped.
        let history = vec![
            text_msg(Role::User, "hello"),
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::Thinking {
                    thinking: "only thinking".to_string(),
                    signature: "sig".to_string(),
                }],
            },
            text_msg(Role::User, "next"),
            text_msg(Role::Assistant, "final"),
        ];

        let view = prepare_context(&history, 1_000_000);
        // The thinking-only message should be gone.
        assert_eq!(view.len(), 3);
    }

    #[test]
    fn tool_results_aged_by_distance() {
        let big = "x".repeat(3000);
        let mut history: Vec<ChatMessage> = Vec::new();

        // Build 20 pairs of user tool-result + assistant response
        for i in 0..20 {
            history.push(tool_result_msg(&format!("t{i}"), &big));
            history.push(text_msg(Role::Assistant, &format!("resp {i}")));
        }

        let view = prepare_context(&history, 1_000_000);
        let len = view.len();

        // Check oldest tool result (distance >= 15) is stubbed
        let oldest_tr = view[0]
            .content
            .iter()
            .find_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .unwrap();
        assert!(
            oldest_tr.starts_with('['),
            "oldest result should be stubbed, got: {}",
            &oldest_tr[..80.min(oldest_tr.len())]
        );

        // Check a mid-range result (distance 5-14) is aged
        let mid_idx = len.saturating_sub(12); // ~distance 11
        if let Some(tr) = view.get(mid_idx) {
            if let Some(content) = tr.content.iter().find_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            }) {
                assert!(
                    content.starts_with("[Aged]"),
                    "mid-range result should be aged, got: {}",
                    &content[..80.min(content.len())]
                );
            }
        }

        // Check recent result (distance < 5) is full
        let recent = &view[len - 2]; // second to last
        if let Some(content) = recent.content.iter().find_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        }) {
            assert_eq!(content.len(), 3000, "recent result should be full");
        }
    }

    #[test]
    fn budget_trimming_drops_oldest() {
        let mut history = Vec::new();
        for i in 0..20 {
            history.push(text_msg(Role::User, &format!("user msg {i}")));
            history.push(text_msg(Role::Assistant, &format!("asst msg {i}")));
        }

        // Tiny budget forces trimming
        let view = prepare_context(&history, 10);

        // Should be significantly trimmed from the original 40 messages
        assert!(view.len() < 40, "should be trimmed, got {} messages", view.len());

        // Check for the budget marker
        let has_marker = view.iter().any(|m| {
            m.content.iter().any(|b| matches!(b, ContentBlock::Text { text } if text.contains("earlier messages removed")))
        });
        assert!(has_marker, "should contain budget removal marker");

        // First message preserved
        if let ContentBlock::Text { text } = &view[0].content[0] {
            assert_eq!(text, "user msg 0");
        }
    }

    #[test]
    fn no_changes_when_within_budget() {
        let history = vec![
            text_msg(Role::User, "hello"),
            text_msg(Role::Assistant, "hi"),
        ];

        let view = prepare_context(&history, 1_000_000);
        assert_eq!(view.len(), 2);
    }
}
