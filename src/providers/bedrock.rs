//! Amazon Bedrock Claude — `invoke_model_with_response_stream` (Messages API shape).

use anyhow::Result;
use aws_config::BehaviorVersion;
use aws_sdk_bedrock::types::ModelModality;
use aws_sdk_bedrockruntime::primitives::Blob;
use aws_sdk_bedrockruntime::types::ResponseStream;
use aws_types::region::Region;
use futures_util::stream;
use tokio::sync::mpsc;

use super::{ChatMessage, RemoteModel, StreamConfig, StreamEvent, ToolDef};
use super::{ANTHROPIC_1M_SUFFIX, anthropic};

async fn load_sdk_config(region: &str, profile: Option<&str>) -> aws_types::SdkConfig {
    let mut loader =
        aws_config::defaults(BehaviorVersion::latest()).region(Region::new(region.to_string()));
    if let Some(p) = profile.filter(|s| !s.trim().is_empty()) {
        loader = loader.profile_name(p.trim());
    }
    loader.load().await
}

/// List foundation models that look like Anthropic Claude chat models with streaming.
pub async fn fetch_bedrock_model_list(
    region: &str,
    profile: Option<&str>,
) -> Result<Vec<RemoteModel>, String> {
    let cfg = load_sdk_config(region, profile).await;
    let client = aws_sdk_bedrock::Client::new(&cfg);
    let out = client
        .list_foundation_models()
        .send()
        .await
        .map_err(|e| format!("Bedrock ListFoundationModels: {e:?}"))?;

    let mut models: Vec<RemoteModel> = Vec::new();
    for m in out.model_summaries() {
        let id = m.model_id();
        let is_anthropic = id.contains("anthropic.")
            || m.provider_name().is_some_and(|p| p.eq_ignore_ascii_case("Anthropic"));
        let text_in = m.input_modalities().contains(&ModelModality::Text);
        let text_out = m.output_modalities().contains(&ModelModality::Text);
        let streams = m.response_streaming_supported().unwrap_or(false);
        if !(is_anthropic && text_in && text_out && streams) {
            continue;
        }

        let label = m
            .model_name()
            .map(str::to_string)
            .unwrap_or_else(|| id.to_string());
        models.push(RemoteModel {
            id: id.to_string(),
            display_name: label,
            context_window: 0,
        });
    }

    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(models)
}

#[allow(clippy::too_many_arguments)]
pub async fn stream(
    region: &str,
    profile: Option<&str>,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    cfg: &StreamConfig,
) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
    let sdk = load_sdk_config(region, profile).await;
    let client = aws_sdk_bedrockruntime::Client::new(&sdk);

    let model_id = model
        .strip_suffix(ANTHROPIC_1M_SUFFIX)
        .unwrap_or(model)
        .to_string();

    let body = anthropic::bedrock_request_body_bytes(
        model_id.as_str(),
        system_prompt,
        messages,
        tools,
        cfg,
    )?;

    let out = client
        .invoke_model_with_response_stream()
        .model_id(model_id.clone())
        .body(Blob::new(body))
        .content_type("application/json")
        .accept("application/json")
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Bedrock invoke_model_with_response_stream: {e:?}"))?;

    let byte_stream = stream::try_unfold(out.body, |mut receiver| async move {
        let r = receiver
            .recv()
            .await
            .map_err(|e| anyhow::anyhow!("Bedrock stream recv: {e:?}"))?;
        let Some(ev) = r else {
            return Ok::<_, anyhow::Error>(None);
        };
        match ev {
            ResponseStream::Chunk(p) => {
                let chunk = p
                    .bytes
                    .as_ref()
                    .map(|b| bytes::Bytes::copy_from_slice(b.as_ref()))
                    .unwrap_or_default();
                Ok(Some((chunk, receiver)))
            }
            // Forward-compatible with SDK variants we do not map yet.
            _ => Ok(Some((bytes::Bytes::new(), receiver))),
        }
    });

    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        if let Err(e) = anthropic::parse_sse_bytes_stream(byte_stream, None, &tx).await {
            let _ = tx.send(StreamEvent::Error {
                message: format!("{e:#}"),
            });
        }
    });

    Ok(rx)
}
