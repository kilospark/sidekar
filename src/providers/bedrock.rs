//! Amazon Bedrock Claude — HTTPS + SigV4 (no `aws-sdk-bedrock` / `aws-sdk-bedrockruntime`).
//!
//! Keeps **`aws-config`** for IAM / SSO / named-profile credential resolution (including the
//! `ProfileFileCredentialsProvider` workaround for env-vs-profile precedence), then signs
//! requests via **`aws-sigv4`** and reads streaming responses as AWS **`application/vnd.amazon.eventstream`**
//! frames. `:event-type` **`chunk`** JSON carries base64 **`bytes`** that concatenate into
//! Anthropic-style SSE, parsed by [`anthropic::parse_sse_bytes_stream`].
//!
//! **HTTP parity** (clone and read `~/src/oss/aws-sdk-rust`): `InvokeModelWithResponseStream` is
//! defined in **`aws-models/bedrock-runtime.json`** — the `accept` input maps to HTTP header
//! **`X-Amzn-Bedrock-Accept`** only (not `Accept`). The model documents default response body type
//! **`application/json`** for the inference payload inside the stream. Request path and SigV4 knobs
//! (`double_uri_encode`, normalized path) match **`sdk/bedrockruntime/src/operation/invoke_model_with_response_stream.rs`**.
//!
//! Inference **`modelId`**: URI label only (`/model/{modelId}/invoke-with-response-stream`). Smithy documents
//! foundation-model id/ARN **or** inference-profile id/ARN; **`ListInferenceProfiles`** is in **`sdk/bedrock/`**
//! (**`~/src/oss/aws-sdk-rust/aws-models/bedrock.json`**).

use anyhow::{Context as _, Result};
use aws_config::{
    BehaviorVersion, Region, SdkConfig, profile::ProfileFileCredentialsProvider,
    provider_config::ProviderConfig,
};
use aws_credential_types::provider::ProvideCredentials as _;
use aws_sigv4::http_request::{SignableBody, SignableRequest, SigningParams, SigningSettings, sign};
use aws_sigv4::sign::v4;
use aws_smithy_eventstream::frame::{DecodedFrame, MessageFrameDecoder};
use aws_smithy_eventstream::smithy;
use aws_smithy_runtime_api::client::identity::Identity;
use base64::Engine as _;
use base64::prelude::BASE64_STANDARD;
use bytes::Bytes;
use futures_util::stream;
use http::Method;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::io::Cursor;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;
use url::Url;

use super::{
    ChatMessage,
    RemoteModel,
    StreamConfig,
    StreamEvent,
    ToolDef,
    ANTHROPIC_1M_SUFFIX,
    anthropic,
    build_streaming_client,
};

async fn load_sdk_config(region: &str, profile: Option<&str>) -> SdkConfig {
    let region = Region::new(region.to_string());
    let mut loader = aws_config::defaults(BehaviorVersion::latest()).region(region.clone());

    if let Some(p) = profile.filter(|s| !s.trim().is_empty()) {
        let pc = ProviderConfig::default().with_region(Some(region.clone()));
        let creds = ProfileFileCredentialsProvider::builder()
            .configure(&pc)
            .profile_name(p.trim())
            .build();
        loader = loader.credentials_provider(creds);
    }

    loader.load().await
}

async fn identity_from_cfg(cfg: &SdkConfig) -> Result<Identity> {
    let provider = cfg.credentials_provider().context("missing AWS credential provider")?;
    let creds = provider
        .provide_credentials()
        .await
        .map_err(|e| anyhow::anyhow!("AWS credentials: {e}"))?;
    Ok(Identity::new(creds.clone(), creds.expiry()))
}

fn signing_params<'a>(
    identity: &'a Identity,
    region: &'a str,
    service_name: &'a str,
) -> Result<SigningParams<'a>> {
    Ok(
        v4::SigningParams::builder()
            .identity(identity)
            .region(region)
            .name(service_name)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .map_err(|e| anyhow::anyhow!("SigV4 build: {e}"))?
            .into(),
    )
}

fn bedrock_invoke_url(region: &str, model_id: &str) -> Result<Url> {
    let mut url = Url::parse(&format!("https://bedrock-runtime.{region}.amazonaws.com/"))
        .map_err(|e| anyhow::anyhow!("Bedrock invoke URL parse: {e}"))?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("cannot build bedrock-runtime path"))?
        .push("model")
        .push(model_id)
        .push("invoke-with-response-stream");
    Ok(url)
}

fn bedrock_control_plane_models_url(region: &str) -> Result<Url> {
    let mut url = Url::parse(&format!("https://bedrock.{region}.amazonaws.com/"))
        .map_err(|e| anyhow::anyhow!("Bedrock URL: {e}"))?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("cannot build bedrock control plane path"))?
        .push("foundation-models");
    Ok(url)
}

/// Bedrock control plane `ListInferenceProfiles` path (Rust SDK parity:
/// `/inference-profiles` + `GET` query `type=SYSTEM_DEFINED`).
const BEDROCK_INFERENCE_PROFILES_PATH: &str = "inference-profiles";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListInferenceProfilesResponse {
    inference_profile_summaries: Option<Vec<InferenceProfileSummaryJson>>,
    next_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InferenceProfileSummaryJson {
    #[serde(default)]
    inference_profile_arn: Option<String>,
    inference_profile_id: String,
    status: Option<String>,
    models: Option<Vec<InferenceProfileModelJson>>,
}

fn bedrock_inference_profile_invoke_ref(s: &InferenceProfileSummaryJson) -> String {
    s.inference_profile_arn
        .as_deref()
        .filter(|a| !a.trim().is_empty())
        .map(std::string::ToString::to_string)
        .unwrap_or_else(|| format!("id:{}", s.inference_profile_id.trim()))
}

/// Paginated `ListInferenceProfiles` (`SYSTEM_DEFINED`). Returns empty on IAM/network/parse failure.
async fn fetch_system_defined_inference_profile_summaries(
    identity: &Identity,
    region: &str,
) -> Vec<InferenceProfileSummaryJson> {
    let Ok(client) = crate::providers::catalog_http_client(120) else {
        return Vec::new();
    };

    let mut next_token = Option::<String>::None;
    let mut out = Vec::<InferenceProfileSummaryJson>::new();

    loop {
        let Ok(mut url) = Url::parse(&format!(
            "https://bedrock.{region}.amazonaws.com/{path}",
            path = BEDROCK_INFERENCE_PROFILES_PATH
        )) else {
            break;
        };
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("type", "SYSTEM_DEFINED");
            q.append_pair("maxResults", "100");
            if let Some(nt) = next_token.as_ref().filter(|s| !s.is_empty()) {
                q.append_pair("nextToken", nt.as_str());
            }
        }

        let Ok(params) = signing_params(identity, region, "bedrock") else {
            break;
        };
        let headers = [("accept", Cow::Borrowed("application/json"))];
        let Ok(http_req) = signed_request(&Method::GET, &url, &headers, &[], &params) else {
            break;
        };
        let Ok(reqwest_req) = reqwest::Request::try_from(http_req) else {
            break;
        };

        let Ok(resp) = client.execute(reqwest_req).await else {
            break;
        };
        if !resp.status().is_success() {
            break;
        }
        let body = resp.bytes().await.unwrap_or_default();
        let Ok(parsed) = serde_json::from_slice::<ListInferenceProfilesResponse>(&body) else {
            break;
        };

        if let Some(sums) = parsed.inference_profile_summaries {
            out.extend(sums);
        }

        match parsed.next_token.filter(|nt| !nt.is_empty()) {
            Some(nt) => next_token = Some(nt),
            None => break,
        }
    }

    out
}

/// Map inference-profile membership foundation-model ARN tails to **`ListFoundationModels` `modelId`**
/// keys (handles `anthropic.foo:version` tails vs bare `anthropic.foo` ids).
fn foundation_model_catalog_keys_from_arn_tail(fm_tail: &str) -> Vec<String> {
    let fm_tail = fm_tail.trim();
    if fm_tail.is_empty() {
        return Vec::new();
    }
    let mut ks = Vec::new();
    ks.push(fm_tail.to_string());
    if let Some((base, _)) = fm_tail.split_once(':') {
        let base = base.trim();
        if !base.is_empty() && base != fm_tail {
            ks.push(base.to_string());
        }
    }
    ks
}

fn inference_profile_invoke_refs_by_foundation_model_id(
    summaries: &[InferenceProfileSummaryJson],
) -> std::collections::HashMap<String, Vec<String>> {
    use std::collections::{HashMap, HashSet};

    let mut map: HashMap<String, HashSet<String>> = HashMap::new();
    for s in summaries {
        if !inference_profile_status_usable(s.status.as_deref()) {
            continue;
        }
        let invoke_ref = bedrock_inference_profile_invoke_ref(s);
        for m in s.models.iter().flatten() {
            let Some(ma) = m.model_arn.as_deref().filter(|a| !a.is_empty()) else {
                continue;
            };
            let Some((_, fm_tail)) = ma.rsplit_once('/') else {
                continue;
            };
            if fm_tail.is_empty() {
                continue;
            }
            for key in foundation_model_catalog_keys_from_arn_tail(fm_tail) {
                map.entry(key)
                    .or_default()
                    .insert(invoke_ref.clone());
            }
        }
    }

    map.into_iter()
        .map(|(k, vs)| {
            let mut refs: Vec<String> = vs.into_iter().collect();
            refs.sort();
            (k, refs)
        })
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InferenceProfileModelJson {
    model_arn: Option<String>,
}

fn inference_profile_arn_covers_foundation_id(arn: &str, foundation_id: &str) -> bool {
    // Typical profile membership: …/foundation-model/anthropic.claude-opus-4-7 (optional :version suffix)
    let tail = arn.rsplit_once('/').map(|(_, t)| t).unwrap_or("");
    tail == foundation_id
        || tail.starts_with(&format!("{foundation_id}:"))
        || tail.starts_with(&format!("{foundation_id}-"))
}

fn inference_profile_summary_covers_foundation(
    summary: &InferenceProfileSummaryJson,
    foundation_id: &str,
) -> bool {
    summary.models.as_ref().is_some_and(|models| {
        models.iter().any(|m| {
            m.model_arn
                .as_ref()
                .is_some_and(|arn| inference_profile_arn_covers_foundation_id(arn, foundation_id))
        })
    })
}

fn inference_profile_status_usable(status: Option<&str>) -> bool {
    match status.map(|s| s.trim()) {
        None | Some("") => true,
        Some(st) => {
            let u = st.to_ascii_uppercase();
            !matches!(u.as_str(), "DEPRECATED" | "DELETED" | "INACTIVE" | "FAILED")
        }
    }
}

/// Resolves **`ListInferenceProfiles`** to a **`modelId` /path segment** value.
///
/// AWS often returns **`inferenceProfileArn`**; for cross-region/system profiles the docs lean on
/// using that ARN as `InvokeModelWithResponseStream` `modelId` (path segment, URL-encoded).
/// Prefer **ARN** when present, then **geo-prefixed `inferenceProfileId`**, then any other match.
async fn resolve_bedrock_invoke_model_identifier(
    identity: &Identity,
    region: &str,
    foundation_model_id: &str,
) -> Option<String> {
    let summaries = fetch_system_defined_inference_profile_summaries(identity, region).await;
    if summaries.is_empty() {
        return None;
    }
    let geo_prefix = format!("{}.", bedrock_geo_inference_prefix(region));

    let mut geo_arn: Option<String> = None;
    let mut geo_id: Option<String> = None;
    let mut any_arn: Option<String> = None;
    let mut any_id: Option<String> = None;

    for s in &summaries {
        if !inference_profile_status_usable(s.status.as_deref()) {
            continue;
        }
        if !inference_profile_summary_covers_foundation(s, foundation_model_id) {
            continue;
        }

        let arn = s
            .inference_profile_arn
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(std::string::ToString::to_string);
        let id = Some(s.inference_profile_id.clone());
        let geo_match = s.inference_profile_id.starts_with(&geo_prefix);

        if geo_match {
            if geo_arn.is_none() {
                geo_arn.clone_from(&arn);
            }
            if geo_id.is_none() {
                geo_id.clone_from(&id);
            }
        }
        if any_arn.is_none() {
            any_arn.clone_from(&arn);
        }
        if any_id.is_none() {
            any_id.clone_from(&id);
        }
    }

    geo_arn.or(geo_id).or(any_arn).or(any_id)
}

fn is_inference_profile_model_id(id: &str) -> bool {
    let id = id.trim();
    id.starts_with("us.")
        || id.starts_with("eu.")
        || id.starts_with("jp.")
        || id.starts_with("global.")
        || id.starts_with("arn:aws:bedrock:")
}

fn bedrock_invoke_model_id_candidates(
    base_model_id: &str,
    region: &str,
    resolved_from_listing: Option<String>,
) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(ref id) = resolved_from_listing
        && !id.is_empty()
    {
        v.push(id.clone());
    }
    if !v.iter().any(|x| x == base_model_id) {
        v.push(base_model_id.to_string());
    }
    if !is_inference_profile_model_id(base_model_id) {
        let geo = format!(
            "{}.{}",
            bedrock_geo_inference_prefix(region),
            base_model_id
        );
        if !v.iter().any(|x| x == &geo) {
            v.push(geo);
        }
    }
    v
}

fn signed_request(
    method: &Method,
    url: &Url,
    header_pairs: &[(&str, Cow<'_, str>)],
    body: &[u8],
    params: &SigningParams<'_>,
) -> Result<http::Request<Bytes>> {
    let hdr_static: Vec<(&str, &str)> = header_pairs.iter().map(|(k, v)| (*k, v.as_ref())).collect();
    let signable = SignableRequest::new(
        method.as_str(),
        url.as_str(),
        hdr_static.into_iter(),
        SignableBody::Bytes(body),
    )
    .map_err(|e| anyhow::anyhow!("SigV4 SignableRequest: {e}"))?;

    let signing_output =
        sign(signable, params).map_err(|e| anyhow::anyhow!("SigV4 sign: {e}"))?;
    let (instructions, _) = signing_output.into_parts();

    let mut builder = http::Request::builder().method(method).uri(url.as_str());
    for (k, v) in header_pairs {
        builder = builder.header(*k, v.as_ref());
    }
    let mut req = builder
        .body(Bytes::copy_from_slice(body))
        .map_err(|e| anyhow::anyhow!("HTTP body: {e}"))?;

    instructions.apply_to_request_http1x(&mut req);
    Ok(req)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListFoundationModelsResponse {
    model_summaries: Vec<FoundationModelSummary>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FoundationModelSummary {
    #[serde(default)]
    model_arn: Option<String>,
    model_id: String,
    model_name: Option<String>,
    provider_name: Option<String>,
    input_modalities: Option<Vec<String>>,
    output_modalities: Option<Vec<String>>,
    response_streaming_supported: Option<bool>,
}

fn modality_contains_text(list: Option<&Vec<String>>) -> bool {
    list.is_some_and(|v| v.iter().any(|s| s.eq_ignore_ascii_case("TEXT")))
}

fn format_list_error(detail_body: &str, status_line: String) -> String {
    let detail = format!("{status_line}: {detail_body}");
    let iam = detail_body.contains("AccessDenied")
        || detail_body.contains("not authorized")
        || detail_body.contains("UnauthorizedOperation");
    let sso_hint =
        detail_body.contains("Session token") || detail_body.contains("switchboard.portal");
    if iam {
        format!(
            "Bedrock ListFoundationModels: {detail} (needs bedrock:ListFoundationModels, or skip listing with `-m` / `/model <id>`)"
        )
    } else if sso_hint {
        format!(
            "Bedrock ListFoundationModels: {detail} \
             (SSO / IAM Identity Center: run `aws sso login --profile <name>`, \
             and unset stray AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY in the REPL environment if you use a named profile)"
        )
    } else {
        format!("Bedrock ListFoundationModels: {detail}")
    }
}

fn utf8_preview(b: &[u8]) -> String {
    std::str::from_utf8(b)
        .map(|s| s.to_string())
        .unwrap_or_else(|_| format!("<binary {} bytes>", b.len()))
}

/// Geographic prefix for Bedrock system inference profiles, from the configured invoke region.
/// See: <https://docs.aws.amazon.com/bedrock/latest/userguide/model-card-anthropic-claude-opus-4-7.html>
fn bedrock_geo_inference_prefix(region: &str) -> &'static str {
    let r = region.trim();
    if r.starts_with("us-") || r.starts_with("ca-") {
        "us"
    } else if r.starts_with("eu-") {
        "eu"
    } else if r == "ap-northeast-1" || r == "ap-northeast-3" {
        "jp"
    } else {
        "global"
    }
}

fn chunk_payload_bytes(payload_json: &[u8]) -> Option<Vec<u8>> {
    let v: Value = serde_json::from_slice(payload_json).ok()?;
    let b64_str = v
        .get("chunk")
        .and_then(|c| c.get("bytes"))
        .and_then(Value::as_str)
        .or_else(|| v.get("bytes").and_then(Value::as_str))?;
    BASE64_STANDARD.decode(b64_str.as_bytes()).ok()
}

/// List foundation models that look like Anthropic Claude chat models with streaming.
pub async fn fetch_bedrock_model_list(
    region: &str,
    profile: Option<&str>,
) -> Result<Vec<RemoteModel>, String> {
    let cfg = load_sdk_config(region, profile).await;
    let identity = identity_from_cfg(&cfg)
        .await
        .map_err(|e| format!("Bedrock IAM: {e}"))?;
    let params =
        signing_params(&identity, region, "bedrock").map_err(|e| format!("SigV4: {e}"))?;
    let url = bedrock_control_plane_models_url(region).map_err(|e| e.to_string())?;

    let headers = [("accept", Cow::Borrowed("application/json"))];
    let http_req = signed_request(&Method::GET, &url, &headers, &[], &params)
        .map_err(|e| e.to_string())?;

    let client = crate::providers::catalog_http_client(120)?;
    let reqwest_req =
        reqwest::Request::try_from(http_req).map_err(|e| format!("reqwest wrap: {e}"))?;

    let resp = client
        .execute(reqwest_req)
        .await
        .map_err(|e| format!("Bedrock ListFoundationModels request: {e}"))?;

    let status = resp.status();
    let resp_bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("reading body: {e}"))?;

    if !status.is_success() {
        let body_txt = utf8_preview(&resp_bytes);
        let line = format!("HTTP {}", status.as_u16());
        return Err(format_list_error(&body_txt, line));
    }

    let parsed: ListFoundationModelsResponse = serde_json::from_slice(&resp_bytes).map_err(|e| {
        format!(
            "Bedrock ListFoundationModels: invalid JSON ({e}); body {}",
            utf8_preview(&resp_bytes).chars().take(256).collect::<String>()
        )
    })?;

    let profile_summaries =
        fetch_system_defined_inference_profile_summaries(&identity, region).await;
    let profile_invoke_refs =
        inference_profile_invoke_refs_by_foundation_model_id(&profile_summaries);

    let mut models: Vec<RemoteModel> = Vec::new();
    for m in parsed.model_summaries {
        let id = &m.model_id;
        let is_anthropic = id.contains("anthropic.")
            || m.provider_name
                .as_ref()
                .is_some_and(|p| p.eq_ignore_ascii_case("Anthropic"));
        let text_in = modality_contains_text(m.input_modalities.as_ref());
        let text_out = modality_contains_text(m.output_modalities.as_ref());
        let streams = m.response_streaming_supported.unwrap_or(false);
        if !(is_anthropic && text_in && text_out && streams) {
            continue;
        }

        let invoke_refs = profile_invoke_refs
            .get(id.as_str())
            .cloned()
            .unwrap_or_default();
        let fm_arn = m
            .model_arn
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(std::string::ToString::to_string);
        let mut row = RemoteModel::catalog(
            id.clone(),
            m.model_name.unwrap_or_else(|| id.clone()),
            0,
        );
        row.bedrock_foundation_model_arn = fm_arn;
        row.bedrock_inference_profile_refs = invoke_refs;
        models.push(row);
    }

    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(models)
}

async fn relay_event_stream_to_channel(
    mut resp: reqwest::Response,
    fwd_tx: mpsc::UnboundedSender<Result<Bytes, anyhow::Error>>,
) {
    fn send_fatal(
        tx: &mpsc::UnboundedSender<Result<Bytes, anyhow::Error>>,
        msg: anyhow::Error,
    ) -> bool {
        tx.send(Err(msg)).is_ok()
    }

    let mut decoder = MessageFrameDecoder::new();
    let mut pending = Vec::<u8>::with_capacity(16 * 1024);

    loop {
        if pending.len() > 256 * 1024 * 1024 {
            let _ =
                fwd_tx.send(Err(anyhow::anyhow!("Bedrock stream decode buffer exceeds limit")));
            return;
        }
        let chunk = match resp.chunk().await {
            Ok(c) => c,
            Err(e) => {
                let _ = send_fatal(&fwd_tx, anyhow::Error::msg(format!(
                    "reading Bedrock invoke stream TCP: {e}"
                )));
                return;
            }
        };
        match chunk {
            Some(b) => pending.extend_from_slice(&b),
            None => break,
        }

        loop {
            let mut cursor = Cursor::new(pending.as_slice());
            match decoder.decode_frame(&mut cursor) {
                Ok(DecodedFrame::Incomplete) => {
                    let consumed = cursor.position() as usize;
                    pending.drain(..consumed);
                    break;
                }
                Ok(DecodedFrame::Complete(msg)) => {
                    let consumed = cursor.position() as usize;

                    match smithy::parse_response_headers(&msg) {
                        Ok(hdr) => {
                            let mt = hdr.message_type.as_str();
                            let evt = hdr.smithy_type.as_str();
                            let payload = msg.payload();

                            if mt == "exception" {
                                let detail = serde_json::from_slice::<Value>(payload.as_ref())
                                    .unwrap_or_else(|_| json!({}));
                                let m = detail
                                    .get("message")
                                    .and_then(|mm| mm.as_str())
                                    .unwrap_or(evt);
                                let full = anyhow::format_err!(
                                    "Bedrock InvokeModel stream: [{evt}] {m}"
                                );
                                let _ = fwd_tx.send(Err(full));
                                pending.drain(..consumed);
                                return;
                            }

                            if mt == "event"
                                && evt.eq_ignore_ascii_case("chunk")
                                && !payload.is_empty()
                                && let Some(bytes) = chunk_payload_bytes(payload.as_ref())
                                && !bytes.is_empty()
                            {
                                let _ = fwd_tx.send(Ok(Bytes::from(bytes)));
                            }
                        }
                        Err(_) => {
                            /* ignore frames we cannot interpret */
                        }
                    }

                    pending.drain(..consumed);
                    continue;
                }
                Err(e) => {
                    let _ =
                        send_fatal(&fwd_tx, anyhow::Error::msg(format!(
                            "Bedrock event-stream frame: {e}"
                        )));
                    return;
                }
            };
        }
    }
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
    let identity = identity_from_cfg(&sdk).await?;
    // Host is bedrock-runtime.*; SigV4 credential scope MUST use signing name `bedrock`
    // (same as aws-sdk-bedrockruntime). Using `bedrock-runtime` yields 403:
    // "Credential should be scoped to correct service: 'bedrock'."

    let base_model_id = model
        .strip_suffix(ANTHROPIC_1M_SUFFIX)
        .unwrap_or(model)
        .to_string();

    let body_vec = anthropic::bedrock_request_body_bytes(
        base_model_id.as_str(),
        system_prompt,
        messages,
        tools,
        cfg,
    )?;

    // InvokeModelWithResponseStream request headers (Smithy `bedrock-runtime.json` —
    // `accept` → `X-Amzn-Bedrock-Accept`; do not send a separate `Accept:` for this operation).
    let header_pairs = [
        ("content-type", Cow::Borrowed("application/json")),
        ("x-amzn-bedrock-accept", Cow::Borrowed("application/json")),
    ];

    let client = build_streaming_client(Duration::from_secs(300))?;

    let resolved = if is_inference_profile_model_id(&base_model_id) {
        None
    } else {
        resolve_bedrock_invoke_model_identifier(&identity, region, &base_model_id).await
    };
    let candidates = bedrock_invoke_model_id_candidates(&base_model_id, region, resolved);

    let mut last_failure: Option<(u16, String, String)> = None;
    let mut http_resp = None;
    for path_model_id in candidates {
        let signing = signing_params(&identity, region, "bedrock")?;
        let url = bedrock_invoke_url(region, &path_model_id)?;
        let http_req = signed_request(&Method::POST, &url, &header_pairs, &body_vec, &signing)?;
        let reqwest_req =
            reqwest::Request::try_from(http_req).map_err(|e| anyhow::anyhow!("{e}"))?;

        let resp = client.execute(reqwest_req).await?;
        let status = resp.status();

        if status.is_success() {
            http_resp = Some(resp);
            break;
        }

        let ct_hint = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let err_body = resp.bytes().await.unwrap_or_default();
        last_failure = Some((status.as_u16(), ct_hint, utf8_preview(&err_body)));
    }

    let http_resp = match http_resp {
        Some(r) => r,
        None => match last_failure {
            Some((code, ct_hint, body)) => anyhow::bail!(
                "Bedrock invoke_model_with_response_stream HTTP {} ({}): {}",
                code,
                ct_hint,
                body
            ),
            None => anyhow::bail!(
                "Bedrock invoke_model_with_response_stream: no response and no error detail"
            ),
        },
    };

    let (fwd_tx, fwd_rx) = mpsc::unbounded_channel::<Result<Bytes, anyhow::Error>>();

    tokio::spawn(async move {
        relay_event_stream_to_channel(http_resp, fwd_tx).await;
        // fwd_tx drops here → receiver sees None after relay returns.
    });

    let byte_stream = stream::try_unfold(fwd_rx, move |mut rx| async move {
        match rx.recv().await {
            None => Ok(None),
            Some(Ok(b)) => Ok(Some((b, rx))),
            Some(Err(e)) => Err(e),
        }
    });

    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        futures_util::pin_mut!(byte_stream);
        if let Err(e) = anthropic::parse_sse_bytes_stream(byte_stream, None, &tx).await {
            let _ = tx.send(StreamEvent::Error {
                message: format!("{e:#}"),
            });
        }
    });

    Ok(rx)
}
