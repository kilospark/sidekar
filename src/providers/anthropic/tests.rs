use super::{CCH_PLACEHOLDER, build_request_body, compute_fingerprint, sign_request_body};
use crate::providers::{ChatMessage, ContentBlock, Role, StreamConfig, ToolDef};
use serde_json::json;

fn test_config() -> StreamConfig {
    StreamConfig::default()
}

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

fn sample_tool(name: &str) -> ToolDef {
    ToolDef {
        name: name.to_string(),
        description: format!("{name} tool"),
        input_schema: json!({"type": "object", "properties": {}}),
    }
}

#[test]
fn build_request_body_places_cache_on_last_tool_when_tools_present() {
    let body = build_request_body(
        "sk-ant-oat01-test",
        "claude-sonnet-4-5",
        "system",
        &[ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "latest".to_string(),
            }],
        }],
        &[
            sample_tool("bash"),
            sample_tool("read"),
            sample_tool("write"),
        ],
        &test_config(),
        false,
    );

    // System should NOT have cache_control when tools are present — we
    // place the stable marker on the last tool instead to guarantee the
    // cached prefix exceeds Anthropic's 1024-token minimum.
    assert!(
        body.system
            .iter()
            .all(|block| block.get("cache_control").is_none()),
        "no system block should have cache_control when tools are present"
    );

    let tools = body.tools.as_ref().expect("tools should be present");
    assert_eq!(tools.len(), 3);
    assert!(
        tools[0].get("cache_control").is_none(),
        "first tool should not have cache_control"
    );
    assert!(
        tools[1].get("cache_control").is_none(),
        "middle tool should not have cache_control"
    );
    assert_eq!(
        tools[2].get("cache_control"),
        Some(&json!({"type": "ephemeral"})),
        "last tool should have the stable cache marker"
    );

    // Rolling marker on the latest message.
    assert_eq!(
        body.messages[0]
            .get("content")
            .and_then(|v| v.as_array())
            .and_then(|parts| parts.last())
            .and_then(|part| part.get("cache_control")),
        Some(&json!({"type": "ephemeral"}))
    );
}

#[test]
fn build_request_body_falls_back_to_system_when_no_tools() {
    let body = build_request_body(
        "sk-ant-oat01-test",
        "claude-sonnet-4-5",
        "system",
        &[ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".to_string(),
            }],
        }],
        &[],
        &test_config(),
        false,
    );

    // Without tools, the stable marker falls back to the last system block.
    assert_eq!(
        body.system
            .last()
            .and_then(|block| block.get("cache_control")),
        Some(&json!({"type": "ephemeral"}))
    );
}

#[test]
fn build_request_body_honors_cache_ttl_from_config() {
    let config = StreamConfig {
        max_tokens: 64_000,
        cache_ttl: Some("1h".into()),
        cache_scope: None,
        ..StreamConfig::default()
    };
    let body = build_request_body(
        "sk-ant-oat01-test",
        "claude-sonnet-4-5",
        "system",
        &[ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".to_string(),
            }],
        }],
        &[],
        &config,
        true,
    );

    assert_eq!(body.max_tokens, 64_000);
    let expected = json!({
        "type": "ephemeral",
        "ttl": "1h",
    });
    assert_eq!(
        body.system
            .last()
            .and_then(|block| block.get("cache_control")),
        Some(&expected),
    );
}

#[test]
fn build_request_body_scope_applies_to_stable_marker_not_messages() {
    // Anthropic rejects `scope` on message-content cache_control but accepts
    // it on system and tool cache_control. This test guards against
    // accidentally stamping scope on message markers and re-triggering the
    // 400 we hit.
    let config = StreamConfig {
        max_tokens: 64_000,
        cache_ttl: Some("1h".into()),
        cache_scope: Some("global".into()),
        ..StreamConfig::default()
    };
    let body = build_request_body(
        "sk-ant-oat01-test",
        "claude-sonnet-4-5",
        "system",
        &[ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".to_string(),
            }],
        }],
        &[sample_tool("bash")],
        &config,
        true,
    );

    // Stable marker lands on the last tool (not system) and includes scope.
    let tools = body.tools.as_ref().expect("tools present");
    assert_eq!(
        tools.last().and_then(|t| t.get("cache_control")),
        Some(&json!({"type": "ephemeral", "ttl": "1h", "scope": "global"})),
    );

    // Message marker must NOT include scope — only ttl.
    let msg_marker = body.messages[0]
        .get("content")
        .and_then(|v| v.as_array())
        .and_then(|parts| parts.last())
        .and_then(|part| part.get("cache_control"))
        .expect("last message should have cache_control");
    assert_eq!(msg_marker, &json!({"type": "ephemeral", "ttl": "1h"}));
    assert!(msg_marker.get("scope").is_none());
}

#[test]
fn build_request_body_converts_oauth_string_content_for_cache_control() {
    let body = build_request_body(
        "sk-ant-oat01-test",
        "claude-sonnet-4-5",
        "system",
        &[ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".to_string(),
            }],
        }],
        &[],
        &test_config(),
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

#[test]
fn supports_1m_context_beta_gates_only_sonnet_4_and_4_5() {
    use crate::providers::supports_1m_context_beta;

    // Sonnet 4/4.5 and Opus 4.5 require the beta header for the 1M tier.
    assert!(supports_1m_context_beta("claude-sonnet-4-20250514"));
    assert!(supports_1m_context_beta("claude-sonnet-4-5-20250929"));
    assert!(supports_1m_context_beta("claude-opus-4-5-20251101"));

    // Sonnet 4.6 and Opus 4.6/4.7 ship 1M as default — no beta gating.
    // But we still surface #1m variants for them (handled in model listing,
    // not in this predicate).
    assert!(!supports_1m_context_beta("claude-sonnet-4-6"));
    assert!(!supports_1m_context_beta("claude-sonnet-4-6-20251015"));
    assert!(!supports_1m_context_beta("claude-opus-4-6"));
    assert!(!supports_1m_context_beta("claude-opus-4-7"));

    // Older / non-eligible models stay off.
    assert!(!supports_1m_context_beta("claude-opus-4-1-20250805"));
    assert!(!supports_1m_context_beta("claude-opus-4-20250514"));
    assert!(!supports_1m_context_beta("claude-haiku-4-5-20251001"));
    assert!(!supports_1m_context_beta("claude-3-5-sonnet-20241022"));
}

#[test]
fn bedrock_invoke_body_strips_stream_field() {
    let bytes = super::build_bedrock_anthropic_messages_request_body(
        "anthropic.claude-3-5-sonnet-20240620-v1:0",
        "system prompt",
        &[ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hi".to_string(),
            }],
        }],
        &[],
        &test_config(),
    )
    .expect("serialize bedrock body");

    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("parse JSON");
    assert!(
        v.get("stream").is_none(),
        "Bedrock InvokeModelWithResponseStream rejects body.stream"
    );
    assert!(v.get("model").is_none());
    assert_eq!(
        v.get("anthropic_version"),
        Some(&json!("bedrock-2023-05-31"))
    );
}
