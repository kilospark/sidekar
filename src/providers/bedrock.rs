//! Amazon Bedrock chat streaming — HTTPS + SigV4 (no `aws-sdk-bedrock` / `aws-sdk-bedrockruntime`).
//!
//! Keeps **`aws-config`** for IAM / SSO / named-profile credential resolution (including the
//! `ProfileFileCredentialsProvider` workaround for env-vs-profile precedence), then signs
//! requests via **`aws-sigv4`** and reads streaming responses as AWS **`application/vnd.amazon.eventstream`**
//! frames. `:event-type` **`chunk`** JSON carries base64 **`bytes`** — inner payload depends on
//! vendor inference family (`AnthropicMessages`, `OpenAiChatCompletions`, `DeepSeekTextCompletion`).
//!
//! **HTTP parity**: Smithy binds only `accept`→`X-Amzn-Bedrock-Accept`, but the **REST** examples in
//! the Amazon Bedrock API Reference also send **`Accept: application/vnd.amazon.eventstream`** so the
//! HTTP response uses the AWS event-stream wrapper; **`X-Amzn-Bedrock-Accept`** selects the MIME type
//! of the inference payloads **inside** that stream (`*/*` or `application/json` per docs).
//!
//! Inference **`modelId`**: URI label only (`/model/{modelId}/invoke-with-response-stream`). Smithy documents
//! foundation-model id/ARN **or** inference-profile id/ARN; **`ListInferenceProfiles`** is in **`sdk/bedrock/`**
//! (**`~/src/oss/aws-sdk-rust/aws-models/bedrock.json`**).

use anyhow::{Context as _, Result};
use aws_config::{
    BehaviorVersion, Region, SdkConfig, profile::ProfileFileCredentialsProvider,
    provider_config::ProviderConfig,
};
use aws_credential_types::{Credentials, provider::ProvideCredentials as _};
use aws_sigv4::http_request::{
    SignableBody, SignableRequest, SigningParams, SigningSettings, sign,
};
use aws_sigv4::sign::v4;
use aws_smithy_eventstream::frame::{DecodedFrame, MessageFrameDecoder};
use aws_smithy_eventstream::smithy;
use aws_smithy_runtime_api::client::identity::Identity;
use base64::Engine as _;
use base64::prelude::BASE64_STANDARD;
use bytes::Bytes;
use futures_util::stream;
use http::Method;
use reqwest::header::{CONTENT_ENCODING, CONTENT_TYPE, HeaderMap, TRANSFER_ENCODING};
use serde::Deserialize;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::mpsc;
use url::Url;

use super::{
    ANTHROPIC_1M_SUFFIX, ChatMessage, RemoteModel, StreamConfig, StreamEvent, ToolDef,
    bedrock_inference, build_streaming_client,
};

const BEDROCK_CREDENTIAL_EXPIRY_SKEW: Duration = Duration::from_secs(300);
const BEDROCK_MODEL_ID_CACHE_TTL: Duration = Duration::from_secs(3600);
/// Bump when inference-profile resolution logic changes so stale IDs are not reused from cache.
const BEDROCK_MODEL_ID_CACHE_SCHEMA: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BedrockCredentialCacheKey {
    region: String,
    profile: Option<String>,
}

#[derive(Debug, Clone)]
struct BedrockCredentialCacheEntry {
    identity: Identity,
    expires_at: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BedrockModelIdCacheKey {
    schema: u32,
    region: String,
    profile: Option<String>,
    foundation_model_id: String,
}

#[derive(Debug, Clone)]
struct BedrockModelIdCacheEntry {
    resolved_model_id: String,
    cached_at: Instant,
}

static BEDROCK_CREDENTIAL_CACHE: LazyLock<
    Mutex<HashMap<BedrockCredentialCacheKey, BedrockCredentialCacheEntry>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

static BEDROCK_MODEL_ID_CACHE: LazyLock<
    Mutex<HashMap<BedrockModelIdCacheKey, BedrockModelIdCacheEntry>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

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

async fn credentials_from_cfg(cfg: &SdkConfig) -> Result<Credentials> {
    let provider = cfg
        .credentials_provider()
        .context("missing AWS credential provider")?;
    provider
        .provide_credentials()
        .await
        .map_err(|e| anyhow::anyhow!("AWS credentials: {e}"))
}

fn credential_cache_key(region: &str, profile: Option<&str>) -> BedrockCredentialCacheKey {
    BedrockCredentialCacheKey {
        region: region.trim().to_string(),
        profile: profile
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    }
}

fn model_id_cache_key(
    region: &str,
    profile: Option<&str>,
    foundation_model_id: &str,
) -> BedrockModelIdCacheKey {
    BedrockModelIdCacheKey {
        schema: BEDROCK_MODEL_ID_CACHE_SCHEMA,
        region: region.trim().to_string(),
        profile: profile
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        foundation_model_id: foundation_model_id.trim().to_string(),
    }
}

fn identity_cache_entry_fresh(entry: &BedrockCredentialCacheEntry) -> bool {
    match entry.expires_at {
        Some(expiry) => expiry
            .duration_since(SystemTime::now())
            .ok()
            .is_some_and(|remaining| remaining > BEDROCK_CREDENTIAL_EXPIRY_SKEW),
        None => true,
    }
}

async fn cached_identity(region: &str, profile: Option<&str>) -> Result<(Identity, bool)> {
    let key = credential_cache_key(region, profile);
    if let Some(entry) = BEDROCK_CREDENTIAL_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
        .cloned()
        && identity_cache_entry_fresh(&entry)
    {
        return Ok((entry.identity, true));
    }

    let sdk = load_sdk_config(region, profile).await;
    let creds = credentials_from_cfg(&sdk).await?;
    let expires_at = creds.expiry();
    let identity = Identity::new(creds.clone(), expires_at);
    let entry = BedrockCredentialCacheEntry {
        identity: identity.clone(),
        expires_at,
    };
    BEDROCK_CREDENTIAL_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, entry);
    Ok((identity, false))
}

async fn cached_resolve_bedrock_invoke_model_identifier(
    identity: &Identity,
    region: &str,
    profile: Option<&str>,
    foundation_model_id: &str,
) -> (Option<String>, bool) {
    let key = model_id_cache_key(region, profile, foundation_model_id);
    if let Some(entry) = BEDROCK_MODEL_ID_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
        .cloned()
        && entry.cached_at.elapsed() < BEDROCK_MODEL_ID_CACHE_TTL
    {
        return (Some(entry.resolved_model_id), true);
    }

    let resolved =
        resolve_bedrock_invoke_model_identifier(identity, region, foundation_model_id).await;
    if let Some(ref resolved_model_id) = resolved {
        BEDROCK_MODEL_ID_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                key,
                BedrockModelIdCacheEntry {
                    resolved_model_id: resolved_model_id.clone(),
                    cached_at: Instant::now(),
                },
            );
    }
    (resolved, false)
}

fn signing_params<'a>(
    identity: &'a Identity,
    region: &'a str,
    service_name: &'a str,
) -> Result<SigningParams<'a>> {
    Ok(v4::SigningParams::builder()
        .identity(identity)
        .region(region)
        .name(service_name)
        .time(SystemTime::now())
        .settings(SigningSettings::default())
        .build()
        .map_err(|e| anyhow::anyhow!("SigV4 build: {e}"))?
        .into())
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
                map.entry(key).or_default().insert(invoke_ref.clone());
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
        let geo = format!("{}.{}", bedrock_geo_inference_prefix(region), base_model_id);
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
    let hdr_static: Vec<(&str, &str)> =
        header_pairs.iter().map(|(k, v)| (*k, v.as_ref())).collect();
    let signable = SignableRequest::new(
        method.as_str(),
        url.as_str(),
        hdr_static.into_iter(),
        SignableBody::Bytes(body),
    )
    .map_err(|e| anyhow::anyhow!("SigV4 SignableRequest: {e}"))?;

    let signing_output = sign(signable, params).map_err(|e| anyhow::anyhow!("SigV4 sign: {e}"))?;
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
    #[allow(dead_code)]
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

fn header_string(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

#[derive(Debug, Clone)]
struct BedrockStreamMeta {
    invoke_model_id: String,
    status: u16,
    content_type: String,
    content_encoding: String,
    transfer_encoding: String,
    request_id: String,
}

impl BedrockStreamMeta {
    fn from_response(resp: &reqwest::Response, invoke_model_id: String) -> Self {
        Self {
            invoke_model_id,
            status: resp.status().as_u16(),
            content_type: header_string(resp.headers(), CONTENT_TYPE.as_str()),
            content_encoding: header_string(resp.headers(), CONTENT_ENCODING.as_str()),
            transfer_encoding: header_string(resp.headers(), TRANSFER_ENCODING.as_str()),
            request_id: header_string(resp.headers(), "x-amzn-requestid"),
        }
    }
}

fn bedrock_stream_eof_message(
    meta: &BedrockStreamMeta,
    total_bytes: usize,
    frames_decoded: usize,
    chunk_events_forwarded: usize,
    pending: &[u8],
) -> String {
    let mut msg = format!(
        "Bedrock invoke_model_with_response_stream ended without decodable content \
         (HTTP {}, modelId {}, content-type {}, request-id {}, bytes {}, frames {}, chunks {})",
        meta.status,
        meta.invoke_model_id,
        if meta.content_type.is_empty() {
            "<missing>"
        } else {
            meta.content_type.as_str()
        },
        if meta.request_id.is_empty() {
            "<missing>"
        } else {
            meta.request_id.as_str()
        },
        total_bytes,
        frames_decoded,
        chunk_events_forwarded
    );

    if !meta.transfer_encoding.is_empty() {
        msg.push_str(&format!(", transfer-encoding {}", meta.transfer_encoding));
    }
    if !meta.content_encoding.is_empty() {
        msg.push_str(&format!(", content-encoding {}", meta.content_encoding));
    }
    if !pending.is_empty() {
        msg.push_str(&format!(
            ", trailing-buffer {}",
            utf8_preview(&pending[..pending.len().min(256)])
        ));
    }

    msg
}

fn unexpected_stream_content_type_message(meta: &BedrockStreamMeta, body: &[u8]) -> String {
    format!(
        "Bedrock invoke_model_with_response_stream unexpected success response \
         (HTTP {}, modelId {}, content-type {}, request-id {}): {}",
        meta.status,
        meta.invoke_model_id,
        if meta.content_type.is_empty() {
            "<missing>"
        } else {
            meta.content_type.as_str()
        },
        if meta.request_id.is_empty() {
            "<missing>"
        } else {
            meta.request_id.as_str()
        },
        utf8_preview(body)
    )
}

/// Geographic prefix for Bedrock system inference profiles, from the configured invoke region.
///
/// Cross-region inference profile IDs use geography prefixes **`us.`**, **`eu.`**, **`apac.`**, and
/// (for Tokyo/Osaka) **`jp.`** — not `global.` ([AWS geographic cross-Region inference][geo]).
/// Mapping everything under `ap-*` except the JP Regions to **`apac`** fixes stale candidates like
/// `global.anthropic…` that Bedrock rejects as “invalid model identifier”.
///
/// [geo]: https://docs.aws.amazon.com/bedrock/latest/userguide/geographic-cross-region-inference.html
fn bedrock_geo_inference_prefix(region: &str) -> &'static str {
    let r = region.trim();
    if r.starts_with("us-") || r.starts_with("ca-") {
        "us"
    } else if r.starts_with("eu-") {
        "eu"
    } else if r == "ap-northeast-1" || r == "ap-northeast-3" {
        "jp"
    } else if r.starts_with("ap-") {
        "apac"
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

/// List Bedrock text-in/text-out streaming models with a sidekar runtime adapter.
pub async fn fetch_bedrock_model_list(
    region: &str,
    profile: Option<&str>,
) -> Result<Vec<RemoteModel>, String> {
    let (identity, _) = cached_identity(region, profile)
        .await
        .map_err(|e| format!("Bedrock IAM: {e}"))?;
    let params = signing_params(&identity, region, "bedrock").map_err(|e| format!("SigV4: {e}"))?;
    let url = bedrock_control_plane_models_url(region).map_err(|e| e.to_string())?;

    let headers = [("accept", Cow::Borrowed("application/json"))];
    let http_req =
        signed_request(&Method::GET, &url, &headers, &[], &params).map_err(|e| e.to_string())?;

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

    let parsed: ListFoundationModelsResponse =
        serde_json::from_slice(&resp_bytes).map_err(|e| {
            format!(
                "Bedrock ListFoundationModels: invalid JSON ({e}); body {}",
                utf8_preview(&resp_bytes)
                    .chars()
                    .take(256)
                    .collect::<String>()
            )
        })?;

    let profile_summaries =
        fetch_system_defined_inference_profile_summaries(&identity, region).await;
    let profile_invoke_refs =
        inference_profile_invoke_refs_by_foundation_model_id(&profile_summaries);

    let mut models: Vec<RemoteModel> = Vec::new();
    for m in parsed.model_summaries {
        let id = &m.model_id;
        let text_in = modality_contains_text(m.input_modalities.as_ref());
        let text_out = modality_contains_text(m.output_modalities.as_ref());
        let streams = m.response_streaming_supported.unwrap_or(false);
        if !(text_in && text_out && streams) {
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
        let mut row =
            RemoteModel::catalog(id.clone(), m.model_name.unwrap_or_else(|| id.clone()), 0);
        row.bedrock_foundation_model_arn = fm_arn;
        row.bedrock_inference_profile_refs = invoke_refs;
        models.push(row);
    }

    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(models)
}

async fn relay_event_stream_to_channel(
    mut resp: reqwest::Response,
    meta: BedrockStreamMeta,
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
    let mut total_bytes = 0usize;
    let mut frames_decoded = 0usize;
    let mut chunk_events_forwarded = 0usize;

    loop {
        if pending.len() > 256 * 1024 * 1024 {
            let _ = fwd_tx.send(Err(anyhow::anyhow!(
                "Bedrock stream decode buffer exceeds limit"
            )));
            return;
        }
        let chunk = match resp.chunk().await {
            Ok(c) => c,
            Err(e) => {
                let _ = send_fatal(
                    &fwd_tx,
                    anyhow::Error::msg(format!("reading Bedrock invoke stream TCP: {e}")),
                );
                return;
            }
        };
        match chunk {
            Some(b) => {
                total_bytes += b.len();
                pending.extend_from_slice(&b);
            }
            None => {
                if chunk_events_forwarded == 0 || !pending.is_empty() {
                    let _ = send_fatal(
                        &fwd_tx,
                        anyhow::Error::msg(bedrock_stream_eof_message(
                            &meta,
                            total_bytes,
                            frames_decoded,
                            chunk_events_forwarded,
                            &pending,
                        )),
                    );
                }
                return;
            }
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
                    frames_decoded += 1;

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
                                let full =
                                    anyhow::format_err!("Bedrock InvokeModel stream: [{evt}] {m}");
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
                                chunk_events_forwarded += 1;
                                let _ = fwd_tx.send(Ok(Bytes::from(bytes)));
                            }
                        }
                        Err(_) => { /* ignore frames we cannot interpret */ }
                    }

                    pending.drain(..consumed);
                    continue;
                }
                Err(e) => {
                    let _ = send_fatal(
                        &fwd_tx,
                        anyhow::Error::msg(format!("Bedrock event-stream frame: {e}")),
                    );
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
    let setup_started = Instant::now();
    let identity_started = Instant::now();
    let (identity, identity_cache_hit) = cached_identity(region, profile).await?;
    let identity_elapsed = identity_started.elapsed();
    // Host is bedrock-runtime.*; SigV4 credential scope MUST use signing name `bedrock`
    // (same as aws-sdk-bedrockruntime). Using `bedrock-runtime` yields 403:
    // "Credential should be scoped to correct service: 'bedrock'."

    let base_model_id = model
        .strip_suffix(ANTHROPIC_1M_SUFFIX)
        .unwrap_or(model)
        .to_string();

    let family = bedrock_inference::infer_bedrock_inference_family(&base_model_id, None);
    let body_vec = bedrock_inference::build_bedrock_invoke_stream_body(
        family,
        base_model_id.as_str(),
        system_prompt,
        messages,
        tools,
        cfg,
    )?;

    // REST sample: `-H accept: application/vnd.amazon.eventstream` + `-H x-amzn-bedrock-accept: */*`
    // (`InvokeModelWithResponseStream` AWS API Reference streaming example).
    let header_pairs = [
        (
            "accept",
            Cow::Borrowed("application/vnd.amazon.eventstream"),
        ),
        ("content-type", Cow::Borrowed("application/json")),
        ("x-amzn-bedrock-accept", Cow::Borrowed("*/*")),
    ];

    let client = build_streaming_client(Duration::from_secs(300))?;

    let resolve_started = Instant::now();
    let (resolved, resolve_cache_hit) = if is_inference_profile_model_id(&base_model_id) {
        (None, true)
    } else {
        cached_resolve_bedrock_invoke_model_identifier(&identity, region, profile, &base_model_id)
            .await
    };
    let resolve_elapsed = resolve_started.elapsed();
    let candidates = bedrock_invoke_model_id_candidates(&base_model_id, region, resolved);

    let mut last_failure: Option<(u16, String, String)> = None;
    let mut http_resp = None;
    let mut selected_path_model_id = None;
    let invoke_started = Instant::now();
    for path_model_id in candidates {
        let signing = signing_params(&identity, region, "bedrock")?;
        let url = bedrock_invoke_url(region, &path_model_id)?;
        let http_req = signed_request(&Method::POST, &url, &header_pairs, &body_vec, &signing)?;
        let reqwest_req =
            reqwest::Request::try_from(http_req).map_err(|e| anyhow::anyhow!("{e}"))?;

        let resp = client.execute(reqwest_req).await?;
        let status = resp.status();

        if status.is_success() {
            selected_path_model_id = Some(path_model_id.clone());
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
    let selected_path_model_id = selected_path_model_id.unwrap_or_else(|| base_model_id.clone());
    let invoke_elapsed = invoke_started.elapsed();
    let resp_meta = BedrockStreamMeta::from_response(&http_resp, selected_path_model_id);
    if super::is_verbose() {
        super::print_verbose_line(&format!(
            "\x1b[2mBedrock setup: creds={}ms ({}) profile-resolve={}ms ({}) invoke={}ms total={}ms model={} target={}\x1b[0m",
            identity_elapsed.as_millis(),
            if identity_cache_hit { "cache" } else { "fresh" },
            resolve_elapsed.as_millis(),
            if resolve_cache_hit { "cache" } else { "fresh" },
            invoke_elapsed.as_millis(),
            setup_started.elapsed().as_millis(),
            base_model_id,
            resp_meta.invoke_model_id,
        ));
    }
    if !resp_meta.content_type.is_empty()
        && !resp_meta
            .content_type
            .to_ascii_lowercase()
            .contains("application/vnd.amazon.eventstream")
    {
        let body = http_resp.bytes().await.unwrap_or_default();
        anyhow::bail!(
            "{}",
            unexpected_stream_content_type_message(&resp_meta, &body)
        );
    }

    let (fwd_tx, fwd_rx) = mpsc::unbounded_channel::<Result<Bytes, anyhow::Error>>();

    tokio::spawn(async move {
        relay_event_stream_to_channel(http_resp, resp_meta, fwd_tx).await;
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
        if let Err(e) =
            bedrock_inference::parse_bedrock_inference_stream(family, byte_stream, &tx).await
        {
            let _ = tx.send(StreamEvent::Error {
                message: format!("{e:#}"),
            });
        }
    });

    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::{
        BedrockStreamMeta, bedrock_invoke_url, bedrock_stream_eof_message,
        unexpected_stream_content_type_message,
    };

    fn meta() -> BedrockStreamMeta {
        BedrockStreamMeta {
            invoke_model_id: "us.anthropic.claude-opus-4-7-20250514-v1:0".to_string(),
            status: 200,
            content_type: "application/json".to_string(),
            content_encoding: String::new(),
            transfer_encoding: "chunked".to_string(),
            request_id: "req-123".to_string(),
        }
    }

    #[test]
    fn eof_message_includes_http_and_model_context() {
        let msg = bedrock_stream_eof_message(&meta(), 0, 0, 0, &[]);
        assert!(msg.contains("HTTP 200"), "{msg}");
        assert!(
            msg.contains("modelId us.anthropic.claude-opus-4-7-20250514-v1:0"),
            "{msg}"
        );
        assert!(msg.contains("content-type application/json"), "{msg}");
        assert!(msg.contains("request-id req-123"), "{msg}");
        assert!(msg.contains("chunks 0"), "{msg}");
    }

    #[test]
    fn eof_message_previews_trailing_utf8() {
        let msg = bedrock_stream_eof_message(&meta(), 17, 0, 0, br#"{"message":"bad"}"#);
        assert!(
            msg.contains(r#"trailing-buffer {"message":"bad"}"#),
            "{msg}"
        );
    }

    #[test]
    fn unexpected_content_type_message_includes_body_preview() {
        let msg = unexpected_stream_content_type_message(&meta(), br#"{"message":"oops"}"#);
        assert!(msg.contains(r#"unexpected success response"#), "{msg}");
        assert!(msg.contains(r#"{"message":"oops"}"#), "{msg}");
    }

    /// Regression: model IDs include `:0`-style suffixes; `http::Uri` must not treat `:0` as a port.
    #[test]
    fn invoke_url_http_uri_path_preserves_colon_in_model_segment() {
        let url = bedrock_invoke_url(
            "us-east-1",
            "anthropic.claude-3-5-sonnet-20240620-v1:0",
        )
        .expect("invoke URL");
        let full = url.as_str();
        let uri: http::Uri = full.parse().expect("parse bedrock runtime URL");
        assert_eq!(
            uri.path(),
            "/model/anthropic.claude-3-5-sonnet-20240620-v1:0/invoke-with-response-stream",
            "unexpected path parsing for {full}"
        );
    }

    /// If `%2F` is decoded into `/` inside `Uri::path()`, ARNs split across segments and Bedrock returns "invalid model identifier".
    #[test]
    fn invoke_url_arn_segment_path_must_not_decode_percent_encoded_slashes() {
        let mid = "arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.anthropic.fake-v1:0";
        let url = bedrock_invoke_url("us-east-1", mid).expect("invoke URL with ARN model id");
        let full = url.as_str();
        let uri: http::Uri = full.parse().expect("parse URL");
        assert!(
            uri.path().contains("%2F"),
            "expected %2F to remain encoded in serialized path for ARN-based model ids; got path={:?} url={full}",
            uri.path()
        );
    }

    #[test]
    fn geo_inference_prefix_maps_most_ap_regions_to_apac() {
        assert_eq!(super::bedrock_geo_inference_prefix("ap-southeast-2"), "apac");
        assert_eq!(super::bedrock_geo_inference_prefix("ap-south-1"), "apac");
        assert_eq!(super::bedrock_geo_inference_prefix("ap-southeast-1"), "apac");
    }

    #[test]
    fn geo_inference_prefix_keeps_japan_regions_as_jp() {
        assert_eq!(super::bedrock_geo_inference_prefix("ap-northeast-1"), "jp");
        assert_eq!(super::bedrock_geo_inference_prefix("ap-northeast-3"), "jp");
    }

    #[test]
    fn geo_inference_prefix_us_ca_eu() {
        assert_eq!(super::bedrock_geo_inference_prefix("us-east-1"), "us");
        assert_eq!(super::bedrock_geo_inference_prefix("ca-central-1"), "us");
        assert_eq!(super::bedrock_geo_inference_prefix("eu-west-1"), "eu");
    }
}
