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

#[test]
fn deepseek_model_enables_compat_thinking_in_request_body() {
    let body = build_request_body(
        "deepseek-v4-pro",
        "",
        &[ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hi".to_string(),
            }],
        }],
        &[],
    );
    assert_eq!(body["thinking"], json!({ "type": "enabled" }));
    assert_eq!(body["reasoning_effort"], json!("high"));
}

#[test]
fn non_deepseek_skips_compat_thinking_field() {
    let body = build_request_body(
        "kimi-k2.5",
        "",
        &[ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hi".to_string(),
            }],
        }],
        &[],
    );
    assert!(
        body.get("thinking").is_none(),
        "thinking is DeepSeek-specific"
    );
}

#[test]
fn reasoning_sse_delta_accepts_nested_object_and_thinking_key() {
    use super::{
        ingest_openai_sse_reasoning_from_delta as ingest_delta,
        ingest_openai_sse_reasoning_from_message as ingest_msg,
    };
    let mut buf = String::new();
    let delta = json!({"reasoning": {"text": "nested"}});
    ingest_delta(&mut buf, &delta);
    assert_eq!(buf, "nested");

    buf.clear();
    ingest_delta(&mut buf, &json!({"thinking": [{"text":"a"},{"text":"b"}]}));
    assert_eq!(buf, "ab");

    buf.clear();
    ingest_msg(
        &mut buf,
        &json!({"role":"assistant","content":"","reasoning_content":"full"}),
    );
    assert_eq!(buf, "full");
}

#[test]
fn deepseek_tool_assistant_always_serializes_reasoning_content() {
    let body = build_request_body(
        "deepseek-v4-pro",
        "",
        &[ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall {
                id: "call_1".into(),
                name: "noop".into(),
                arguments: json!({}),
                thought_signature: None,
            }],
        }],
        &[],
    );
    let messages = body.get("messages").and_then(|m| m.as_array()).unwrap();
    let asst = &messages[0];
    assert!(asst.get("tool_calls").is_some());
    assert!(
        asst.get("reasoning_content").is_some(),
        "DeepSeek + tools requires reasoning_content field"
    );
    assert_eq!(asst["reasoning_content"], "");
}

#[test]
fn deepseek_tool_assistant_maps_pre_tool_plain_text_into_reasoning_content() {
    let body = build_request_body(
        "deepseek-v4-pro",
        "",
        &[ChatMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "planning excerpt".into(),
                },
                ContentBlock::ToolCall {
                    id: "call_1".into(),
                    name: "noop".into(),
                    arguments: json!({}),
                    thought_signature: None,
                },
            ],
        }],
        &[],
    );
    let messages = body.get("messages").and_then(|m| m.as_array()).unwrap();
    assert_eq!(messages[0]["reasoning_content"], "planning excerpt");
    assert_eq!(messages[0]["content"], "planning excerpt");
}
