use super::*;
use crate::providers::{ChatMessage, ContentBlock, Role};
use serde_json::json;

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

fn tool_call_msg(id: &str, name: &str, args: serde_json::Value) -> ChatMessage {
    ChatMessage {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args,
            thought_signature: None,
        }],
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

/// Build a complete tool cycle: assistant ToolCall + user ToolResult.
fn tool_cycle(id: &str, name: &str, args: serde_json::Value, result: &str) -> Vec<ChatMessage> {
    vec![tool_call_msg(id, name, args), tool_result_msg(id, result)]
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

    prepare_context(&history, 1_000_000);

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
    let view = prepare_context(&history, 10);

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

    prepare_context(&history, 1_000_000);
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
        prepare_context(&history, 1_000_000);
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
    let history = vec![
        text_msg(Role::User, "hello"),
        text_msg(Role::Assistant, "hi"),
    ];

    let view = prepare_context(&history, 1_000_000);
    assert_eq!(view.len(), 2);
}

// -----------------------------------------------------------------------
// Tool cycle aging tests
// -----------------------------------------------------------------------

#[test]
fn age_old_tool_cycles_stubs_old_results() {
    let big = "x".repeat(500);
    let big_args = json!({"content": "y".repeat(500)});
    let mut history: Vec<ChatMessage> = Vec::new();

    // 8 tool cycles — with keep=3, the oldest 5 should be aged.
    for i in 0..8 {
        history.extend(tool_cycle(&format!("t{i}"), "Read", big_args.clone(), &big));
    }

    let mut view = history.clone();
    age_old_tool_cycles(&mut view, 3);

    // Oldest 5 cycles = 10 messages (indices 0..10): results and args should be stubbed
    for msg in &view[..10] {
        for block in &msg.content {
            match block {
                ContentBlock::ToolResult { content, .. } => {
                    assert!(
                        content.starts_with("[tool output cleared"),
                        "old result should be stubbed, got: {content}"
                    );
                    assert!(
                        content.contains("tool: Read"),
                        "stub should include tool name"
                    );
                }
                ContentBlock::ToolCall { arguments, .. } => {
                    assert_eq!(
                        arguments,
                        &json!({}),
                        "old call arguments should be cleared"
                    );
                }
                _ => {}
            }
        }
    }

    // Last 3 cycles (indices 10..16): intact
    for msg in &view[10..] {
        for block in &msg.content {
            match block {
                ContentBlock::ToolResult { content, .. } => {
                    assert_eq!(content, &big, "recent result must be intact");
                }
                ContentBlock::ToolCall { arguments, .. } => {
                    assert_eq!(arguments, &big_args, "recent call args must be intact");
                }
                _ => {}
            }
        }
    }
}

#[test]
fn age_old_tool_cycles_skips_small_content() {
    let small = "ok";
    let small_args = json!({"cmd": "ls"});
    let mut history: Vec<ChatMessage> = Vec::new();

    for i in 0..8 {
        history.extend(tool_cycle(
            &format!("t{i}"),
            "Bash",
            small_args.clone(),
            small,
        ));
    }

    let mut view = history.clone();
    age_old_tool_cycles(&mut view, 3);

    // Small content should not be touched even in old cycles
    for msg in &view {
        for block in &msg.content {
            match block {
                ContentBlock::ToolResult { content, .. } => {
                    assert_eq!(content, small, "small result should not be aged");
                }
                ContentBlock::ToolCall { arguments, .. } => {
                    assert_eq!(arguments, &small_args, "small args should not be aged");
                }
                _ => {}
            }
        }
    }
}

#[test]
fn age_old_tool_cycles_noop_when_fewer_than_keep() {
    let big = "x".repeat(500);
    let mut history: Vec<ChatMessage> = Vec::new();

    // Only 3 cycles, keep=5 → no aging
    for i in 0..3 {
        history.extend(tool_cycle(&format!("t{i}"), "Grep", json!({}), &big));
    }

    let mut view = history.clone();
    age_old_tool_cycles(&mut view, 5);

    // Everything should be intact
    for (a, b) in view.iter().zip(history.iter()) {
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }
}

#[test]
fn age_old_tool_cycles_canonical_history_untouched() {
    // Verify that prepare_context does not mutate the original history
    // even when tool cycle aging runs.
    let big = "x".repeat(500);
    let big_args = json!({"path": "/".to_string() + &"a".repeat(500)});
    let mut history: Vec<ChatMessage> = Vec::new();

    for i in 0..10 {
        history.extend(tool_cycle(&format!("t{i}"), "Read", big_args.clone(), &big));
    }

    let snapshot: Vec<String> = history
        .iter()
        .flat_map(|m| {
            m.content.iter().filter_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
        })
        .collect();

    prepare_context(&history, 1_000_000);

    let after: Vec<String> = history
        .iter()
        .flat_map(|m| {
            m.content.iter().filter_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
        })
        .collect();

    assert_eq!(
        snapshot, after,
        "canonical history must not be mutated by prepare_context"
    );
}

#[test]
fn age_boundary_stable_across_user_messages() {
    // The key cache property: adding plain user/assistant text messages
    // should NOT shift the aging boundary. Only new tool cycles shift it.
    let big = "x".repeat(500);
    let mut history: Vec<ChatMessage> = Vec::new();

    // 6 tool cycles
    for i in 0..6 {
        history.extend(tool_cycle(&format!("t{i}"), "Read", json!({}), &big));
    }

    let view1 = prepare_context(&history, 1_000_000);
    let aged_count_1 = count_aged_results(&view1);

    // Add 5 plain text turns (no tool calls)
    for i in 0..5 {
        history.push(text_msg(Role::User, &format!("question {i}")));
        history.push(text_msg(Role::Assistant, &format!("answer {i}")));
    }

    let view2 = prepare_context(&history, 1_000_000);
    let aged_count_2 = count_aged_results(&view2);

    assert_eq!(
        aged_count_1, aged_count_2,
        "adding text messages must not change which tool results are aged"
    );
}

#[test]
fn age_old_tool_cycles_mixed_content_assistant_msg() {
    // An assistant message can have both Text and ToolCall blocks.
    // Only the ToolCall arguments should be aged; text should be preserved.
    let big_args = json!({"content": "z".repeat(500)});
    let big = "x".repeat(500);
    let mut history: Vec<ChatMessage> = Vec::new();

    // 7 cycles with mixed assistant messages (text + tool call)
    for i in 0..7 {
        let id = format!("t{i}");
        history.push(ChatMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: format!("I'll read file {i}"),
                },
                ContentBlock::ToolCall {
                    id: id.clone(),
                    name: "Read".to_string(),
                    arguments: big_args.clone(),
                    thought_signature: None,
                },
            ],
        });
        history.push(tool_result_msg(&id, &big));
    }

    let mut view = history.clone();
    age_old_tool_cycles(&mut view, 3);

    // Old cycles: text preserved, tool call args cleared
    for msg in &view[..8] {
        // first 4 cycles = 8 messages
        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => {
                    assert!(
                        text.starts_with("I'll read file"),
                        "text blocks must be preserved"
                    );
                }
                ContentBlock::ToolCall {
                    arguments, name, ..
                } => {
                    assert_eq!(arguments, &json!({}), "old args should be cleared");
                    assert_eq!(name, "Read", "tool name must be preserved");
                }
                ContentBlock::ToolResult { content, .. } => {
                    assert!(
                        content.starts_with("[tool output cleared"),
                        "old result should be stubbed"
                    );
                }
                _ => {}
            }
        }
    }
}

/// Count how many ToolResult blocks in the view have been aged (contain the stub marker).
fn count_aged_results(view: &[ChatMessage]) -> usize {
    view.iter()
        .flat_map(|m| m.content.iter())
        .filter(|b| matches!(b, ContentBlock::ToolResult { content, .. } if content.starts_with("[tool output cleared")))
        .count()
}
