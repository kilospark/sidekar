use super::{apply_usage, build_request_body};
use crate::providers::{ChatMessage, ContentBlock, Role, StreamConfig, Usage};
use serde_json::json;

fn test_config() -> StreamConfig {
    StreamConfig::default()
}

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
        None,
        &test_config(),
    );

    assert_eq!(
        body.get("prompt_cache_key").and_then(|v| v.as_str()),
        Some("sess-123")
    );
}

#[test]
fn build_request_body_sets_store_false_with_encrypted_reasoning() {
    let body = build_request_body(
        "gpt-5.4",
        "system",
        &[ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hi".to_string(),
            }],
        }],
        &[],
        None,
        None,
        &test_config(),
    );

    assert_eq!(body.get("store").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        body.get("include").and_then(|v| v.as_array()),
        Some(&vec![json!("reasoning.encrypted_content")])
    );
}

// NOTE: previous_response_id is plumbed through the call chain but NOT yet
// included in the HTTP POST body because chatgpt.com's POST endpoint rejects
// it (WebSocket only). These tests verify the parameter flows correctly; the
// body assertion is commented out until WS transport is implemented.
#[test]
fn build_request_body_omits_previous_response_id_with_store_false() {
    let body = build_request_body(
        "gpt-5.4",
        "system",
        &[ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hi".to_string(),
            }],
        }],
        &[],
        None,
        Some("resp_abc123"),
        &test_config(),
    );

    // previous_response_id is incompatible with store:false
    assert!(body.get("previous_response_id").is_none());
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

#[test]
fn build_request_body_replays_encrypted_reasoning() {
    let body = build_request_body(
        "gpt-5.4",
        "system",
        &[
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "hello".to_string(),
                }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::EncryptedReasoning {
                        encrypted_content: "opaque-blob-123".to_string(),
                        summary: vec![json!({"type": "summary_text", "text": "thought about it"})],
                    },
                    ContentBlock::Text {
                        text: "Hi there!".to_string(),
                    },
                ],
            },
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "follow up".to_string(),
                }],
            },
        ],
        &[],
        None,
        None,
        &test_config(),
    );

    let input = body.get("input").and_then(|v| v.as_array()).unwrap();
    // Should contain: user msg, reasoning, assistant msg, user msg
    let reasoning_item = input
        .iter()
        .find(|item| item.get("type").and_then(|v| v.as_str()) == Some("reasoning"))
        .expect("reasoning item should be present in input");
    assert_eq!(
        reasoning_item.get("encrypted_content").and_then(|v| v.as_str()),
        Some("opaque-blob-123")
    );
    assert!(reasoning_item.get("summary").is_some());
    // Reasoning should come before the assistant text message
    let reasoning_idx = input
        .iter()
        .position(|item| item.get("type").and_then(|v| v.as_str()) == Some("reasoning"))
        .unwrap();
    let assistant_msg_idx = input
        .iter()
        .position(|item| {
            item.get("type").and_then(|v| v.as_str()) == Some("message")
                && item.get("role").and_then(|v| v.as_str()) == Some("assistant")
        })
        .unwrap();
    assert!(
        reasoning_idx < assistant_msg_idx,
        "reasoning should precede assistant message"
    );
}

#[test]
fn build_request_body_includes_codex_config_fields() {
    use crate::providers::ReasoningConfig;

    let config = StreamConfig {
        parallel_tool_calls: true,
        temperature: Some(1.0),
        reasoning: Some(ReasoningConfig {
            effort: "high".into(),
            summary: "auto".into(),
        }),
        text_verbosity: Some("verbose".into()),
        ..StreamConfig::default()
    };

    let body = build_request_body(
        "gpt-5.4",
        "system",
        &[ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hi".to_string(),
            }],
        }],
        &[crate::providers::ToolDef {
            name: "bash".into(),
            description: "run commands".into(),
            input_schema: json!({"type": "object", "properties": {}}),
        }],
        None,
        None,
        &config,
    );

    assert_eq!(
        body.get("parallel_tool_calls").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        body.get("temperature").and_then(|v| v.as_f64()),
        Some(1.0)
    );
    assert_eq!(
        body.get("reasoning"),
        Some(&json!({"effort": "high", "summary": "auto"}))
    );
    assert_eq!(
        body.get("text"),
        Some(&json!({"verbosity": "verbose"}))
    );
}
