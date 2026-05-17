//! Routing for Bedrock **`InvokeModelWithResponseStream`** — request JSON + inner chunk parser
//! differ by vendor wire family.

use anyhow::{Context as _, Result, bail};
use futures_util::{StreamExt, pin_mut};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{
    AssistantResponse, ChatMessage, ContentBlock, Role, StopReason, StreamConfig, StreamEvent,
    ToolDef, Usage, anthropic, openrouter,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BedrockInferenceFamily {
    AnthropicMessages,
    OpenAiChatCompletions,
    DeepSeekTextCompletion,
}

pub(crate) fn infer_bedrock_inference_family(
    model_id: &str,
    provider_name: Option<&str>,
) -> BedrockInferenceFamily {
    let prov_lc = provider_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let m = model_id.trim().to_ascii_lowercase();

    if prov_lc == "deepseek" || m.contains("deepseek") {
        return BedrockInferenceFamily::DeepSeekTextCompletion;
    }
    if prov_lc == "openai" || m.starts_with("openai.") {
        return BedrockInferenceFamily::OpenAiChatCompletions;
    }
    // NVIDIA Nemotron on Bedrock uses InvokeModel + OpenAI-shaped JSON (`messages`,
    // `max_tokens`, …), not Anthropic Messages — see AWS blog “Run NVIDIA Nemotron 3 Super”.
    if prov_lc == "nvidia"
        || prov_lc.contains("nvidia")
        || m.starts_with("nvidia.")
        || m.contains("nemotron")
    {
        return BedrockInferenceFamily::OpenAiChatCompletions;
    }
    // Qwen on Bedrock validates requests like OpenAI Chat Completions (e.g. each tool needs
    // top-level `"type": "function"`). Anthropic Messages-shaped tools trigger validation_error.
    if prov_lc == "qwen" || m.starts_with("qwen.") {
        return BedrockInferenceFamily::OpenAiChatCompletions;
    }
    // Z.ai GLM on Bedrock: same OpenAI-compat tool/message constraints as Qwen.
    if m.starts_with("zai.") || prov_lc == "z.ai" || prov_lc == "zai" {
        return BedrockInferenceFamily::OpenAiChatCompletions;
    }
    // Mistral models use OpenAI-style streaming payloads inside Bedrock event chunks; routing them
    // through Anthropic JSON-event decoding yields validationException / parse failures mid-stream.
    if m.starts_with("mistral.") || m.starts_with("mistralai.") {
        return BedrockInferenceFamily::OpenAiChatCompletions;
    }
    if prov_lc == "mistral ai" || prov_lc == "mistral" {
        return BedrockInferenceFamily::OpenAiChatCompletions;
    }
    // Moonshot Kimi on Bedrock: OpenAI Chat Completions wire format; Anthropic-shaped `tools`
    // triggers validation_error ("missing field `type`").
    if m.starts_with("moonshotai.") || prov_lc.contains("moonshot") {
        return BedrockInferenceFamily::OpenAiChatCompletions;
    }
    // MiniMax on Bedrock: same OpenAI-compat constraints as Qwen / Nemotron.
    if m.starts_with("minimax.") || prov_lc.contains("minimax") {
        return BedrockInferenceFamily::OpenAiChatCompletions;
    }
    BedrockInferenceFamily::AnthropicMessages
}

pub(crate) fn build_bedrock_invoke_stream_body(
    family: BedrockInferenceFamily,
    model_id: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    cfg: &StreamConfig,
) -> Result<Vec<u8>> {
    match family {
        BedrockInferenceFamily::AnthropicMessages => {
            anthropic::build_bedrock_anthropic_messages_request_body(
                model_id,
                system_prompt,
                messages,
                tools,
                cfg,
            )
        }
        BedrockInferenceFamily::OpenAiChatCompletions => {
            let mut body = openrouter::openai_compat_chat_completion_body(
                model_id,
                system_prompt,
                messages,
                tools,
            );
            let obj = body
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("Bedrock OpenAI body must be JSON object"))?;
            // Model id is only in the InvokeModel path segment; duplicate `model` in JSON
            // confuses some Bedrock vendors (AWS Nemotron samples omit it).
            obj.remove("model");
            // `InvokeModelWithResponseStream` streams via the REST route; bodies that include
            // OpenAI's `stream` / `stream_options` hit strict validators ("stream: Extra inputs
            // are not permitted") — mirror Anthropic Messages handling in `build_bedrock_anthropic_*`.
            obj.remove("stream");
            obj.remove("stream_options");
            // Third-party OpenAI-compat models often have ~128k total context; Sidekar's Bedrock
            // default max output (64k) plus a large system+history prompt exceeds that (e.g.
            // Nemotron: 67k in + 64k out > 131072).
            let max = bedrock_openai_max_tokens_clamped(
                model_id,
                system_prompt,
                messages,
                tools,
                cfg.max_tokens,
            );
            obj.insert("max_completion_tokens".into(), json!(max));
            // NVIDIA / legacy Bedrock samples use `max_tokens` (AWS CLI Nemotron guides).
            obj.insert("max_tokens".into(), json!(max));
            if let Some(t) = cfg.temperature {
                obj.insert("temperature".into(), json!(t));
            }
            serde_json::to_vec(&body).map_err(anyhow::Error::from)
        }
        BedrockInferenceFamily::DeepSeekTextCompletion => {
            if !tools.is_empty() {
                bail!(
                    "Bedrock DeepSeek Invoke completion path does not support tools in Sidekar yet"
                );
            }
            validate_deepseek_plain_text_messages(messages)?;
            let prompt = flatten_deepseek_prompt(system_prompt, messages)?;
            let max_tok = cfg.max_tokens.min(8192).max(1);
            let mut body = serde_json::Map::new();
            body.insert("prompt".into(), json!(prompt));
            body.insert("max_tokens".into(), json!(max_tok));
            if let Some(t) = cfg.temperature {
                body.insert("temperature".into(), json!(t));
            }
            serde_json::to_vec(&Value::Object(body)).map_err(anyhow::Error::from)
        }
    }
}

/// Conservative context ceiling for Bedrock vendors using the OpenAI-shaped Invoke body.
fn bedrock_openai_context_ceiling(model_id: &str) -> u32 {
    let m = model_id.trim().to_ascii_lowercase();
    if m.starts_with("openai.") {
        return 200_000;
    }
    // NVIDIA Nemotron 3, Moonshot Kimi, MiniMax, many Meta/Mistral SKUs: ~128k combined budget.
    if m.starts_with("nvidia.")
        || m.contains("nemotron")
        || m.starts_with("moonshotai.")
        || m.starts_with("minimax.")
        || m.starts_with("meta.")
        || m.starts_with("mistral")
    {
        return 131_072;
    }
    131_072
}

/// Wire-size budget for tool definitions (not represented as ChatMessage content).
fn rough_tool_defs_char_budget(tools: &[ToolDef]) -> usize {
    let mut chars: usize = 0;
    for t in tools {
        chars = chars
            .saturating_add(t.name.len())
            .saturating_add(t.description.len())
            .saturating_add(t.input_schema.to_string().len())
            .saturating_add(256);
    }
    chars
}

fn rough_bedrock_message_chars(system_prompt: &str, messages: &[ChatMessage]) -> usize {
    let mut chars: usize = system_prompt.len();
    for msg in messages {
        for block in &msg.content {
            chars = chars.saturating_add(match block {
                ContentBlock::Text { text } => text.len(),
                ContentBlock::Thinking { thinking, .. } => thinking.len(),
                ContentBlock::ToolCall { arguments, .. } => {
                    arguments.to_string().len().saturating_add(96)
                }
                ContentBlock::ToolResult {
                    content,
                    content_images,
                    ..
                } => content.len().saturating_add(48).saturating_add(
                    content_images
                        .iter()
                        .map(|i| i.data_base64.len().saturating_div(4).saturating_add(256))
                        .sum(),
                ),
                ContentBlock::Image { data_base64, .. } => {
                    data_base64.len().saturating_div(4).saturating_add(256)
                }
                ContentBlock::EncryptedReasoning {
                    encrypted_content,
                    summary,
                } => {
                    let s: usize = summary.iter().map(|v| v.to_string().len()).sum();
                    encrypted_content.len().saturating_add(s)
                }
                ContentBlock::Reasoning { text } => text.len(),
            });
        }
    }
    chars
}

/// Pessimistic billable input estimate: Bedrock's tokenizer + JSON-wrapped `messages`/`tools`
/// often exceeds naive `chars/4`; tool-only validation errors (column ~13k) mean schema size matters.
fn bedrock_combined_input_tokens_pessimistic(
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
) -> u32 {
    let c_msgs = rough_bedrock_message_chars(system_prompt, messages);
    let c_tools = rough_tool_defs_char_budget(tools);
    let chars = c_msgs.saturating_add(c_tools);
    // OpenAI-style request JSON expands with keys/quotes/escapes — stay above Bedrock's count.
    let base = (chars / 3).max(1) as u64;
    // ~33% headroom vs chars/3: matches cases where server reports ~35% more than chars/4.
    let boosted = base.saturating_mul(4).saturating_div(3);
    boosted.min(u64::from(u32::MAX)) as u32
}

fn bedrock_openai_max_tokens_clamped(
    model_id: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    requested: u32,
) -> u32 {
    let ceiling = bedrock_openai_context_ceiling(model_id);
    const MARGIN: u32 = 4096;
    let est_in = bedrock_combined_input_tokens_pessimistic(system_prompt, messages, tools);
    let room = ceiling.saturating_sub(est_in).saturating_sub(MARGIN);
    if room == 0 {
        return 1;
    }
    requested.min(room).max(1)
}

fn validate_deepseek_plain_text_messages(messages: &[ChatMessage]) -> Result<()> {
    for msg in messages {
        for block in &msg.content {
            match block {
                ContentBlock::Text { .. } => {}
                _ => bail!(
                    "Bedrock DeepSeek completion supports text-only turns in Sidekar (non-text block)"
                ),
            }
        }
    }
    Ok(())
}

fn flatten_deepseek_prompt(system_prompt: &str, messages: &[ChatMessage]) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    if !system_prompt.trim().is_empty() {
        parts.push(format!("System:\n{}", system_prompt.trim()));
    }
    for msg in messages {
        let role = match msg.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
        };
        let mut text = String::new();
        for block in &msg.content {
            if let ContentBlock::Text { text: t } = block {
                text.push_str(t);
            }
        }
        if text.trim().is_empty() {
            continue;
        }
        parts.push(format!("{role}:\n{text}"));
    }
    if parts.is_empty() {
        bail!("Bedrock DeepSeek prompt would be empty");
    }
    Ok(parts.join("\n\n"))
}

pub(crate) async fn parse_bedrock_inference_stream<S>(
    family: BedrockInferenceFamily,
    stream: S,
    tx: &mpsc::UnboundedSender<StreamEvent>,
) -> Result<()>
where
    S: futures_util::Stream<Item = std::result::Result<bytes::Bytes, anyhow::Error>> + Send,
{
    match family {
        BedrockInferenceFamily::AnthropicMessages => {
            anthropic::parse_json_event_bytes_stream(stream, None, tx).await
        }
        BedrockInferenceFamily::OpenAiChatCompletions => {
            openrouter::parse_openai_completion_chunk_byte_stream(stream, None, tx).await
        }
        BedrockInferenceFamily::DeepSeekTextCompletion => {
            parse_deepseek_completion_chunk_byte_stream(stream, tx).await
        }
    }
}

async fn parse_deepseek_completion_chunk_byte_stream<S>(
    stream: S,
    tx: &mpsc::UnboundedSender<StreamEvent>,
) -> Result<()>
where
    S: futures_util::Stream<Item = std::result::Result<bytes::Bytes, anyhow::Error>> + Send,
{
    pin_mut!(stream);
    let mut full_text = String::new();
    let mut usage = Usage::default();
    let mut finish_reason: Option<String> = None;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error reading Bedrock DeepSeek chunk stream")?;
        let Ok(data) = serde_json::from_slice::<Value>(chunk.as_ref()) else {
            continue;
        };

        if let Some(msg) = openrouter::openai_compat_stream_error_message(&data) {
            bail!("{msg}");
        }

        if let Some(u) = data.get("usage") {
            openrouter::apply_usage(u, &mut usage);
        }

        if let Some(fr) = data
            .pointer("/choices/0/stop_reason")
            .and_then(|v| v.as_str())
        {
            finish_reason = Some(fr.to_string());
        }

        if let Some(delta) = extract_deepseek_chunk_text_delta(&data) {
            if !delta.is_empty() {
                full_text.push_str(delta);
                let _ = tx.send(StreamEvent::TextDelta {
                    delta: delta.to_string(),
                });
            }
        }
    }

    let stop = match finish_reason.as_deref() {
        Some("length") => StopReason::Length,
        _ => StopReason::Stop,
    };

    let mut content = Vec::new();
    if !full_text.is_empty() {
        content.push(ContentBlock::Text { text: full_text });
    }

    let _ = tx.send(StreamEvent::Done {
        message: AssistantResponse {
            content,
            usage,
            stop_reason: stop,
            model: String::new(),
            response_id: String::new(),
            rate_limit: None,
        },
    });

    Ok(())
}

fn extract_deepseek_chunk_text_delta(v: &Value) -> Option<&str> {
    v.pointer("/choices/0/delta/content")
        .and_then(|x| x.as_str())
        .or_else(|| v.pointer("/choices/0/text").and_then(|x| x.as_str()))
        .or_else(|| v.pointer("/choices/0/delta/text").and_then(|x| x.as_str()))
        .or_else(|| v.get("generation").and_then(|x| x.as_str()))
        .or_else(|| v.pointer("/outputs/0/text").and_then(|x| x.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_family_prefers_provider_metadata() {
        assert_eq!(
            infer_bedrock_inference_family("foo", Some("DeepSeek")),
            BedrockInferenceFamily::DeepSeekTextCompletion
        );
        assert_eq!(
            infer_bedrock_inference_family("foo", Some("OpenAI")),
            BedrockInferenceFamily::OpenAiChatCompletions
        );
        assert_eq!(
            infer_bedrock_inference_family("foo", Some("Z.ai")),
            BedrockInferenceFamily::OpenAiChatCompletions
        );
        assert_eq!(
            infer_bedrock_inference_family("foo", Some("Mistral AI")),
            BedrockInferenceFamily::OpenAiChatCompletions
        );
        assert_eq!(
            infer_bedrock_inference_family("foo", Some("Anthropic")),
            BedrockInferenceFamily::AnthropicMessages
        );
        assert_eq!(
            infer_bedrock_inference_family("foo", Some("Moonshot AI")),
            BedrockInferenceFamily::OpenAiChatCompletions
        );
        assert_eq!(
            infer_bedrock_inference_family("foo", Some("MiniMax")),
            BedrockInferenceFamily::OpenAiChatCompletions
        );
        assert_eq!(
            infer_bedrock_inference_family("foo", Some("NVIDIA")),
            BedrockInferenceFamily::OpenAiChatCompletions
        );
    }

    #[test]
    fn infer_family_heuristic_model_id() {
        assert_eq!(
            infer_bedrock_inference_family("us.deepseek.r1-v1:0", None),
            BedrockInferenceFamily::DeepSeekTextCompletion
        );
        assert_eq!(
            infer_bedrock_inference_family("openai.gpt-oss-20b-1:0", None),
            BedrockInferenceFamily::OpenAiChatCompletions
        );
        assert_eq!(
            infer_bedrock_inference_family("nvidia.nemotron-super-3-120b", None),
            BedrockInferenceFamily::OpenAiChatCompletions
        );
        assert_eq!(
            infer_bedrock_inference_family("qwen.qwen3-coder-next", None),
            BedrockInferenceFamily::OpenAiChatCompletions
        );
        assert_eq!(
            infer_bedrock_inference_family("zai.glm-4.7", None),
            BedrockInferenceFamily::OpenAiChatCompletions
        );
        assert_eq!(
            infer_bedrock_inference_family("mistral.mistral-large-2407-v1:0", None),
            BedrockInferenceFamily::OpenAiChatCompletions
        );
        assert_eq!(
            infer_bedrock_inference_family("anthropic.claude-3-5-sonnet-20240620-v1:0", None),
            BedrockInferenceFamily::AnthropicMessages
        );
        assert_eq!(
            infer_bedrock_inference_family("moonshotai.kimi-k2.5", None),
            BedrockInferenceFamily::OpenAiChatCompletions
        );
        assert_eq!(
            infer_bedrock_inference_family("minimax.minimax-m2.5", None),
            BedrockInferenceFamily::OpenAiChatCompletions
        );
        assert_eq!(
            infer_bedrock_inference_family("us.amazon.nemotron-super-3-120b-1:0", None),
            BedrockInferenceFamily::OpenAiChatCompletions
        );
    }

    #[test]
    fn bedrock_openai_invoke_body_omits_stream_key() {
        let cfg = super::super::StreamConfig::default();
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hi".to_string(),
            }],
        }];
        let bytes = build_bedrock_invoke_stream_body(
            BedrockInferenceFamily::OpenAiChatCompletions,
            "openai.gpt-oss-20b-1:0",
            "",
            &messages,
            &[],
            &cfg,
        )
        .expect("body");
        let v: serde_json::Value = serde_json::from_slice(&bytes).expect("body must be JSON");
        assert!(
            v.get("stream").is_none(),
            "InvokeModelWithResponseStream uses route for streaming; top-level \"stream\" is rejected ({v})"
        );
        assert!(
            v.get("stream_options").is_none(),
            "OpenAI stream_options must not be sent on Bedrock stream invoke ({v})"
        );
    }

    #[test]
    fn bedrock_openai_clamps_max_tokens_when_history_is_large() {
        let mut cfg = super::super::StreamConfig::default();
        cfg.max_tokens = 64_000;
        let huge = "x".repeat(270_000);
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: huge }],
        }];
        let bytes = build_bedrock_invoke_stream_body(
            BedrockInferenceFamily::OpenAiChatCompletions,
            "nvidia.nemotron-super-3-120b",
            "",
            &messages,
            &[],
            &cfg,
        )
        .expect("body");
        let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        let max = v
            .get("max_tokens")
            .and_then(|x| x.as_u64())
            .expect("max_tokens");
        assert!(max < 64_000, "expected clamp from 64k, got {max}");
    }

    #[test]
    fn bedrock_openai_clamps_when_tool_schemas_are_huge() {
        use serde_json::json;

        let mut cfg = super::super::StreamConfig::default();
        cfg.max_tokens = 64_000;
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hi".to_string(),
            }],
        }];
        let tools = vec![super::super::ToolDef {
            name: "bash".to_string(),
            description: "shell".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pad": { "type": "string", "description": "x".repeat(200_000) }
                }
            }),
        }];
        let bytes = build_bedrock_invoke_stream_body(
            BedrockInferenceFamily::OpenAiChatCompletions,
            "nvidia.nemotron-super-3-120b",
            "",
            &messages,
            &tools,
            &cfg,
        )
        .expect("body");
        let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        let max = v
            .get("max_tokens")
            .and_then(|x| x.as_u64())
            .expect("max_tokens");
        assert!(max < 64_000, "tool bulk must lower max_tokens, got {max}");
    }
}
