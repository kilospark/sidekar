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
fn tool_results_preserved_across_turns() {
    // Regression: progressive aging used to mutate historical tool_results
    // in-place once they crossed a distance threshold, destroying the prompt
    // cache key at that position. prepare_context must leave tool_result
    // content untouched on canonical history so the cache prefix is stable.
    let big = "x".repeat(3000);
    let mut history: Vec<ChatMessage> = Vec::new();

    for i in 0..20 {
        history.push(tool_result_msg(&format!("t{i}"), &big));
        history.push(text_msg(Role::Assistant, &format!("resp {i}")));
    }

    let before: Vec<String> = history
        .iter()
        .flat_map(|m| {
            m.content.iter().filter_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
        })
        .collect();

    prepare_context(&mut history, 1_000_000);

    let after: Vec<String> = history
        .iter()
        .flat_map(|m| {
            m.content.iter().filter_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
        })
        .collect();

    assert_eq!(before, after, "canonical history must not be mutated");
    for content in &after {
        assert_eq!(content.len(), 3000, "tool result content must be intact");
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
fn tool_results_stable_across_new_turns() {
    // Regression: adding new turns used to push older messages past the
    // distance threshold and rewrite them, breaking the cache. The canonical
    // history at positions [0..N) must stay byte-identical after new turns
    // are appended and prepare_context is called again.
    let big = "x".repeat(3000);
    let mut history: Vec<ChatMessage> = Vec::new();
    for i in 0..10 {
        history.push(tool_result_msg(&format!("t{i}"), &big));
        history.push(text_msg(Role::Assistant, &format!("resp {i}")));
    }

    prepare_context(&mut history, 1_000_000);
    let snapshot: Vec<String> = history
        .iter()
        .flat_map(|m| {
            m.content.iter().filter_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
        })
        .collect();

    for i in 0..5 {
        history.push(text_msg(Role::User, &format!("q{i}")));
        history.push(text_msg(Role::Assistant, &format!("a{i}")));
        prepare_context(&mut history, 1_000_000);
    }

    let current: Vec<String> = history
        .iter()
        .take(20)
        .flat_map(|m| {
            m.content.iter().filter_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
        })
        .collect();

    assert_eq!(snapshot, current, "historical tool_results must not mutate");
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
