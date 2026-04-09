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
    let mut history = vec![
        text_msg(Role::User, "hello"),
        thinking_msg("old reasoning", "response 1"),
        text_msg(Role::User, "next"),
        thinking_msg("recent reasoning", "response 2"),
    ];

    let view = prepare_context(&mut history, 1_000_000);

    // First assistant: thinking stripped, only text remains
    let first_asst = view
        .iter()
        .find(|m| {
            m.role == Role::Assistant
                && m.content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Text { text } if text == "response 1"))
        })
        .expect("first assistant message should exist");
    assert!(
        !first_asst
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Thinking { .. })),
        "thinking should be stripped from non-last assistant"
    );

    // Last assistant: thinking preserved
    let last_asst = view.last().unwrap();
    assert!(
        last_asst
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Thinking { .. })),
        "thinking should be preserved on last assistant"
    );
}

#[test]
fn empty_messages_removed_after_eviction() {
    // An assistant message with only a thinking block should be dropped.
    let mut history = vec![
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

    let view = prepare_context(&mut history, 1_000_000);
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

    let view = prepare_context(&mut history, 1_000_000);
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
    let view = prepare_context(&mut history, 10);

    // Should be significantly trimmed from the original 40 messages
    assert!(
        view.len() < 40,
        "should be trimmed, got {} messages",
        view.len()
    );

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
fn aged_results_stay_stable_across_turns() {
    let big = "x".repeat(3000);
    let mut history: Vec<ChatMessage> = Vec::new();

    // Build 20 pairs so some results are well past the aging thresholds.
    for i in 0..20 {
        history.push(tool_result_msg(&format!("t{i}"), &big));
        history.push(text_msg(Role::Assistant, &format!("resp {i}")));
    }

    // First call — ages in-place
    prepare_context(&mut history, 1_000_000);

    // Snapshot the canonical history after aging.
    let snapshot: Vec<String> = history
        .iter()
        .flat_map(|m| {
            m.content.iter().filter_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
        })
        .collect();

    // Simulate several new turns to shift distances.
    for i in 0..5 {
        history.push(text_msg(Role::User, &format!("q{i}")));
        history.push(text_msg(Role::Assistant, &format!("a{i}")));
        prepare_context(&mut history, 1_000_000);
    }

    // All results that were already aged must be byte-identical.
    let current: Vec<String> = history
        .iter()
        .take(40) // original 20 pairs
        .flat_map(|m| {
            m.content.iter().filter_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
        })
        .collect();

    for (i, (before, after)) in snapshot.iter().zip(current.iter()).enumerate() {
        if before.starts_with("[Aged]") || before.starts_with("[") && before.contains(" bytes]")
        {
            assert_eq!(
                before, after,
                "already-aged tool result t{i} changed on subsequent turn"
            );
        }
    }
}

#[test]
fn no_changes_when_within_budget() {
    let mut history = vec![
        text_msg(Role::User, "hello"),
        text_msg(Role::Assistant, "hi"),
    ];

    let view = prepare_context(&mut history, 1_000_000);
    assert_eq!(view.len(), 2);
}
