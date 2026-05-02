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
    if prov_lc == "nvidia" || m.starts_with("nvidia.") {
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
            obj.insert("stream".into(), json!(true));
            obj.remove("stream_options");
            let max = cfg.max_tokens;
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
            infer_bedrock_inference_family("foo", Some("Anthropic")),
            BedrockInferenceFamily::AnthropicMessages
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
            infer_bedrock_inference_family("anthropic.claude-3-5-sonnet-20240620-v1:0", None),
            BedrockInferenceFamily::AnthropicMessages
        );
    }
}
