use super::{apply_usage, build_request_body};
use crate::providers::{ChatMessage, ContentBlock, Role, Usage};
use serde_json::json;

#[test]
fn build_request_body_adds_openrouter_cache_control_for_claude() {
    let body = build_request_body(
        "anthropic/claude-sonnet-4-5",
        "system",
        &[ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".to_string(),
            }],
        }],
        &[],
    );

    let messages = body
        .get("messages")
        .and_then(|v| v.as_array())
        .expect("messages array");
    let content = messages[1]
        .get("content")
        .and_then(|v| v.as_array())
        .expect("content array");
    assert_eq!(
        content[0].get("cache_control"),
        Some(&json!({"type": "ephemeral"}))
    );
}

#[test]
fn apply_usage_extracts_cached_tokens() {
    let usage_json = json!({
        "prompt_tokens": 900,
        "completion_tokens": 42,
        "prompt_tokens_details": {
            "cached_tokens": 300,
            "cache_write_tokens": 50
        }
    });
    let mut usage = Usage::default();

    apply_usage(&usage_json, &mut usage);

    assert_eq!(usage.input_tokens, 550);
    assert_eq!(usage.output_tokens, 42);
    assert_eq!(usage.cache_read_tokens, 300);
    assert_eq!(usage.cache_write_tokens, 50);
}
