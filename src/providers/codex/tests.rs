use super::{apply_usage, build_request_body};
use crate::providers::{ChatMessage, ContentBlock, Role, Usage};
use serde_json::json;

#[test]
fn build_request_body_includes_prompt_cache_key() {
    let body = build_request_body(
        "gpt-5.4",
        "system",
        &[ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".to_string(),
            }],
        }],
        &[],
        Some("sess-123"),
    );

    assert_eq!(
        body.get("prompt_cache_key").and_then(|v| v.as_str()),
        Some("sess-123")
    );
}

#[test]
fn apply_usage_extracts_cached_token_details() {
    let usage_json = json!({
        "input_tokens": 1200,
        "output_tokens": 55,
        "input_tokens_details": {
            "cached_tokens": 400,
            "cache_creation_tokens": 100
        }
    });
    let mut usage = Usage::default();

    apply_usage(&usage_json, &mut usage);

    assert_eq!(usage.input_tokens, 700);
    assert_eq!(usage.output_tokens, 55);
    assert_eq!(usage.cache_read_tokens, 400);
    assert_eq!(usage.cache_write_tokens, 100);
}
