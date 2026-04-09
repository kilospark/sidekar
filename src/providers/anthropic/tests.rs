use super::{CCH_PLACEHOLDER, build_request_body, compute_fingerprint, sign_request_body};
use crate::providers::{ChatMessage, ContentBlock, Role};
use serde_json::json;

#[test]
fn fingerprint_matches_reference_example() {
    let prompt = "Say 'hello' and nothing else.";
    assert_eq!(compute_fingerprint(prompt, "2.1.37"), "9e7");
}

#[test]
fn sign_request_body_replaces_only_the_first_placeholder() {
    let body = format!("{{\"system\":\"{CCH_PLACEHOLDER}\",\"messages\":\"{CCH_PLACEHOLDER}\"}}");
    let signed = sign_request_body(&body);

    assert!(!signed.contains(&format!("\"system\":\"{CCH_PLACEHOLDER}\"")));
    assert!(signed.contains(&format!("\"messages\":\"{CCH_PLACEHOLDER}\"")));
}

#[test]
fn build_request_body_adds_cache_control_to_system_and_tail_messages() {
    let body = build_request_body(
        "claude-sonnet-4-5",
        "system",
        &[
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "first".to_string(),
                }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "second".to_string(),
                }],
            },
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: "tool result".to_string(),
                    is_error: false,
                }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolCall {
                    id: "call_1".to_string(),
                    name: "bash".to_string(),
                    arguments: json!({"cmd": "pwd"}),
                }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "latest".to_string(),
                }],
            },
        ],
        &[],
        16_000,
        false,
    );

    assert_eq!(
        body.system
            .last()
            .and_then(|block| block.get("cache_control")),
        Some(&json!({"type": "ephemeral"}))
    );
    assert_eq!(body.messages.len(), 5);
    assert!(
        body.messages[0]
            .get("content")
            .and_then(|v| v.as_array())
            .and_then(|parts| { parts.last().and_then(|part| part.get("cache_control")) })
            .is_none()
    );
    assert_eq!(
        body.messages[1]
            .get("content")
            .and_then(|v| v.as_array())
            .and_then(|parts| parts.last())
            .and_then(|part| part.get("cache_control")),
        Some(&json!({"type": "ephemeral"}))
    );
    assert_eq!(
        body.messages[2]
            .get("content")
            .and_then(|v| v.as_array())
            .and_then(|parts| parts.last())
            .and_then(|part| part.get("cache_control")),
        Some(&json!({"type": "ephemeral"}))
    );
    assert!(
        body.messages[3]
            .get("content")
            .and_then(|v| v.as_array())
            .and_then(|parts| { parts.last().and_then(|part| part.get("cache_control")) })
            .is_none()
    );
    assert_eq!(
        body.messages[4]
            .get("content")
            .and_then(|v| v.as_array())
            .and_then(|parts| parts.last())
            .and_then(|part| part.get("cache_control")),
        Some(&json!({"type": "ephemeral"}))
    );
}

#[test]
fn build_request_body_converts_oauth_string_content_for_cache_control() {
    let body = build_request_body(
        "claude-sonnet-4-5",
        "system",
        &[ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".to_string(),
            }],
        }],
        &[],
        16_000,
        true,
    );

    let content = body.messages[0]
        .get("content")
        .and_then(|v| v.as_array())
        .expect("oauth text content should be converted to block array");
    assert_eq!(
        content[0].get("text").and_then(|v| v.as_str()),
        Some("hello")
    );
    assert_eq!(
        content[0].get("cache_control"),
        Some(&json!({"type": "ephemeral"}))
    );
}
