//! Google Vertex AI OpenAI-compatible endpoint adapter.
//!
//! Vertex's OpenAI-compat surface lives at regional hosts
//! `{LOC}-aiplatform.googleapis.com` or host `aiplatform.googleapis.com` when `LOC` is `global`:
//!   `/v1/projects/<PROJECT>/locations/<LOC>/endpoints/openapi`
//!
//! Partner Claude on Vertex also exposes `:rawPredict` / `:streamRawPredict` on publisher model paths;
//! see [`is_vertex_anthropic_partner_models_base`] and [`anthropic_partner_stream_url`].
//!
//! Quirks vs. plain OpenAI-compat that this module handles centrally:
//!
//! 1. `/models` is **not** implemented at the openapi base (returns 404 HTML).
//!    The actual model catalog lives at
//!    `https://aiplatform.googleapis.com/v1beta1/publishers/<pub>/models`
//!    and must be queried per publisher with the
//!    `x-goog-user-project: <PROJECT>` header.
//!
//! 2. The **`openapi` Chat Completions** path (`…/chat/completions`) supports **Gemini**
//!    and select self-hosted Model Garden containers — **not** Anthropic Claude.
//!    Claude on Vertex uses **publisher-model** `:streamRawPredict` instead; Sidekar
//!    redirects `anthropic/<id>` automatically ([overview](https://cloud.google.com/vertex-ai/generative-ai/docs/migrate/openai/overview)).
//!
//! 3. For Gemini via openapi, model IDs must be `<publisher>/<model-id>`
//!    (e.g. `google/gemini-2.5-flash`). Bare `gemini-2.5-flash` returns 400.
//!    The publisher-models response uses `publishers/<pub>/models/<id>` —
//!    we transform to the `<pub>/<id>` form expected by the chat endpoint.
//!
//! 4. Auth uses `Authorization: Bearer <token>` where `<token>` is a GCP OAuth2 access token (`ya29.*`).
//!    Store **`adc`** on the OpenAI-compat credential to resolve tokens via Application Default Credentials
//!    ([`crate::providers::gcp_adc`]): `GOOGLE_APPLICATION_CREDENTIALS`, `gcloud auth application-default login`,
//!    or GCE metadata. Alternatively paste a token from `gcloud auth print-access-token`.
//!    The `x-goog-user-project` header is required on the model-listing endpoint to attribute quota/billing.

use serde_json::Value;

use super::{RemoteModel, is_verbose};

/// Publishers we probe for model discovery. Vertex doesn't expose a
/// publisher-listing endpoint, so we fan out and merge. Each request returns
/// 200 even when the project hasn't enabled that publisher (with `models: []`),
/// so unknown publishers are cheap.
const KNOWN_PUBLISHERS: &[&str] = &[
    "google",
    "anthropic",
    "meta",
    "meta-llama",
    "mistralai",
    "qwen",
    "deepseek-ai",
    "ai21",
    "cohere",
];

/// True when `base_url` points at Vertex AI's OpenAI-compat openapi endpoint.
pub fn is_vertex_openapi_base(base_url: &str) -> bool {
    let lower = base_url.to_ascii_lowercase();
    lower.contains("aiplatform.googleapis.com") && lower.contains("/endpoints/openapi")
}

/// Base URL for Vertex AI OpenAI-compatible Chat Completions (`…/chat/completions`).
///
/// `location` is normalized to lowercase for the hostname and path (e.g. `us-central1`).
/// **`global`** uses host `aiplatform.googleapis.com`; regional locations use `{loc}-aiplatform.googleapis.com`.
pub fn openapi_endpoint_base(project_id: &str, location: &str) -> String {
    let proj = project_id.trim();
    let loc = location.trim().to_ascii_lowercase();
    let host = if loc == "global" {
        "aiplatform.googleapis.com".to_string()
    } else {
        format!("{loc}-aiplatform.googleapis.com")
    };
    format!("https://{host}/v1/projects/{proj}/locations/{loc}/endpoints/openapi")
}

/// Publisher-model Claude on Vertex
/// (`…/publishers/anthropic/models/<MODEL>`, optional `:rawPredict` / `:streamRawPredict` suffix).
///
/// Requires a regional hostname (for example `us-east5-aiplatform.googleapis.com`).
/// Streaming REPL traffic should use [`anthropic_partner_stream_url`].
pub fn is_vertex_anthropic_partner_models_base(base_url: &str) -> bool {
    let lower = base_url.to_ascii_lowercase();
    lower.contains("aiplatform.googleapis.com") && lower.contains("/publishers/anthropic/models/")
}

/// Model id segment from the URL path (`claude-opus-4` from `…/models/claude-opus-4:rawPredict`).
pub fn anthropic_partner_model_id(base_url: &str) -> Option<String> {
    let trimmed = base_url.trim_end_matches('/');
    let lower = trimmed.to_ascii_lowercase();
    let path_end = lower
        .rfind(":streamrawpredict")
        .or_else(|| lower.rfind(":rawpredict"))
        .unwrap_or(trimmed.len());
    let path = &trimmed[..path_end];
    let needle = "/models/";
    let idx = path.find(needle)?;
    let tail = &path[idx + needle.len()..];
    if tail.is_empty() {
        return None;
    }
    Some(tail.to_string())
}

/// Vertex `openapi` Chat Completions does not serve Claude ([Google docs](https://cloud.google.com/vertex-ai/generative-ai/docs/migrate/openai/overview)).
/// Map to publisher-model base (`…/publishers/anthropic/models/<id>`) using the same scheme/host/project/location.
///
/// Accepts catalog IDs (`anthropic/claude-…`) or bare Vertex Claude IDs (`claude-opus-4`, …).
pub fn openapi_base_to_anthropic_partner_base(base_url: &str, model: &str) -> Option<String> {
    if !is_vertex_openapi_base(base_url) {
        return None;
    }
    let model_clean = model
        .strip_suffix(super::ANTHROPIC_1M_SUFFIX)
        .unwrap_or(model)
        .trim();
    let bare_id = anthropic_partner_bare_model_id(model_clean)?;
    let parsed = url::Url::parse(base_url.trim_end_matches('/')).ok()?;
    let scheme = parsed.scheme();
    let host = parsed.host_str()?;
    let project = extract_project(base_url)?;
    let location = extract_location_from_vertex_path(parsed.path())?;
    Some(format!(
        "{scheme}://{host}/v1/projects/{project}/locations/{location}/publishers/anthropic/models/{bare_id}"
    ))
}

/// Bare publisher model segment for Claude (`claude-haiku-4-5`) or full `anthropic/<id>` from catalog.
fn anthropic_partner_bare_model_id(model_clean: &str) -> Option<&str> {
    if let Some((pub_id, bare)) = model_clean.split_once('/') {
        if pub_id == "anthropic" && !bare.is_empty() {
            return Some(bare);
        }
        return None;
    }
    let lower = model_clean.to_ascii_lowercase();
    if lower.starts_with("claude-") && !model_clean.contains('/') {
        return Some(model_clean);
    }
    None
}

fn extract_location_from_vertex_path(path: &str) -> Option<String> {
    let rest = path.split("/locations/").nth(1)?;
    let loc = rest.split('/').next()?;
    if loc.is_empty() {
        None
    } else {
        Some(loc.to_string())
    }
}

/// `:streamRawPredict` URL for streaming (see [Google's REST
/// docs](https://cloud.google.com/vertex-ai/generative-ai/docs/partner-models/claude/use-claude)).
pub fn anthropic_partner_stream_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let lower = trimmed.to_ascii_lowercase();
    let base_no_method = if let Some(i) = lower.rfind(":streamrawpredict") {
        &trimmed[..i]
    } else if let Some(i) = lower.rfind(":rawpredict") {
        &trimmed[..i]
    } else {
        trimmed
    };
    format!("{base_no_method}:streamRawPredict")
}

/// Extract the GCP project ID from a Vertex openapi base URL.
/// Returns `None` if the URL doesn't match the expected shape.
pub fn extract_project(base_url: &str) -> Option<String> {
    // Path shape: /v1/projects/<PROJECT>/locations/<LOC>/endpoints/openapi
    let after_projects = base_url.split("/projects/").nth(1)?;
    let project = after_projects.split('/').next()?;
    if project.is_empty() {
        None
    } else {
        Some(project.to_string())
    }
}

/// Fetch the union of all publisher models available to the given project.
/// Empty Vec on any auth/network failure (caller falls back to manual entry).
pub async fn fetch_models(api_key: &str, base_url: &str) -> Vec<RemoteModel> {
    let verbose = is_verbose();
    let project = match extract_project(base_url) {
        Some(p) => p,
        None => {
            if verbose {
                crate::broker::try_log_event(
                    "debug",
                    "vertex",
                    "extract-project-failed",
                    Some(base_url),
                );
            }
            return Vec::new();
        }
    };

    let client = match super::catalog_http_client(15) {
        Ok(c) => c,
        Err(e) => {
            if verbose {
                crate::broker::try_log_error(
                    "vertex",
                    "failed to build http client",
                    Some(&format!("{e:#}")),
                );
            }
            return Vec::new();
        }
    };

    // Fan out per publisher in parallel.
    let futures = KNOWN_PUBLISHERS.iter().map(|pub_id| {
        let client = client.clone();
        let api_key = api_key.to_string();
        let project = project.clone();
        let pub_id = pub_id.to_string();
        async move {
            let url = format!(
                "https://aiplatform.googleapis.com/v1beta1/publishers/{pub_id}/models?pageSize=200"
            );
            let resp = match client
                .get(&url)
                .header("authorization", format!("Bearer {api_key}"))
                .header("x-goog-user-project", &project)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    if verbose {
                        crate::broker::try_log_error(
                            "vertex",
                            &format!("model list request failed: {url}"),
                            Some(&format!("{e:#}")),
                        );
                    }
                    return Vec::new();
                }
            };
            if !resp.status().is_success() {
                if verbose {
                    let status = resp.status();
                    crate::broker::try_log_event(
                        "debug",
                        "vertex",
                        "model-list-http-error",
                        Some(&format!("url={url} status={status}")),
                    );
                }
                return Vec::new();
            }
            let body: Value = match resp.json().await {
                Ok(v) => v,
                Err(_) => return Vec::new(),
            };
            parse_publisher_models(&pub_id, &body)
        }
    });

    let results: Vec<Vec<RemoteModel>> = futures_util::future::join_all(futures).await;
    let mut out: Vec<RemoteModel> = results.into_iter().flatten().collect();

    // Stable sort by id for deterministic UI ordering.
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// Parse a v1beta1 publisher-models response into our `RemoteModel` shape,
/// transforming model IDs from `publishers/<pub>/models/<id>` to `<pub>/<id>`
/// so they're ready to send straight to Vertex's openapi `/chat/completions`.
fn parse_publisher_models(publisher: &str, body: &Value) -> Vec<RemoteModel> {
    let arr = match body.get("publisherModels").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    for m in arr {
        // `name` is `publishers/<pub>/models/<id>`. Strip prefix to get bare id.
        let raw_name = m.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let bare_id = raw_name.rsplit('/').next().unwrap_or("").to_string();
        if bare_id.is_empty() {
            continue;
        }
        // Skip non-chat / non-text models. Vertex marks these via
        // `supportedActions` / `launchStage`, but the simplest heuristic is to
        // exclude obvious non-LLM IDs (embeddings, image, video, tts).
        if is_non_chat(&bare_id) {
            continue;
        }
        let chat_id = format!("{publisher}/{bare_id}");
        let display = m
            .get("versionId")
            .and_then(|v| v.as_str())
            .map(|v| format!("{chat_id} ({v})"))
            .unwrap_or_else(|| chat_id.clone());
        out.push(RemoteModel::catalog(chat_id, display, 0));
    }
    out
}

fn is_non_chat(id: &str) -> bool {
    let l = id.to_ascii_lowercase();
    l.contains("embedding")
        || l.contains("imagegeneration")
        || l.contains("imagen")
        || l.contains("videogeneration")
        || l.contains("veo")
        || l.contains("text-bison")
        || l.contains("textembedding")
        || l.starts_with("chirp")
        || l.starts_with("tts")
        || l.contains("medlm")
        || l.contains("code-gecko")
        || l.contains("translation")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_vertex_openapi_base() {
        assert!(is_vertex_openapi_base(
            "https://aiplatform.googleapis.com/v1/projects/foo/locations/global/endpoints/openapi"
        ));
        assert!(is_vertex_openapi_base(
            "https://aiplatform.googleapis.com/v1/projects/foo/locations/us-central1/endpoints/openapi/"
        ));
        // Wrong host
        assert!(!is_vertex_openapi_base("https://api.openai.com/v1"));
        // Right host, wrong path
        assert!(!is_vertex_openapi_base(
            "https://aiplatform.googleapis.com/v1/projects/foo/locations/global"
        ));
    }

    #[test]
    fn extracts_project_from_base_url() {
        assert_eq!(
            extract_project(
                "https://aiplatform.googleapis.com/v1/projects/vertex-test-project/locations/global/endpoints/openapi"
            ),
            Some("vertex-test-project".to_string())
        );
        assert_eq!(
            extract_project(
                "https://aiplatform.googleapis.com/v1/projects/my-proj/locations/us-central1/endpoints/openapi/"
            ),
            Some("my-proj".to_string())
        );
        assert_eq!(
            extract_project(
                "https://us-west1-aiplatform.googleapis.com/v1/projects/vertex-test-project/locations/us-west1/publishers/anthropic/models/claude-mock-model:rawPredict"
            ),
            Some("vertex-test-project".to_string())
        );
        assert_eq!(extract_project("https://api.openai.com/v1"), None);
    }

    #[test]
    fn openapi_base_maps_to_anthropic_partner_for_claude_models() {
        let openapi = "https://us-west1-aiplatform.googleapis.com/v1beta1/projects/vertex-test-project/locations/us-west1/endpoints/openapi";
        assert_eq!(
            openapi_base_to_anthropic_partner_base(openapi, "anthropic/claude-mock-model"),
            Some(
                "https://us-west1-aiplatform.googleapis.com/v1/projects/vertex-test-project/locations/us-west1/publishers/anthropic/models/claude-mock-model".to_string()
            )
        );
        assert_eq!(
            openapi_base_to_anthropic_partner_base(openapi, "claude-mock-model"),
            Some(
                "https://us-west1-aiplatform.googleapis.com/v1/projects/vertex-test-project/locations/us-west1/publishers/anthropic/models/claude-mock-model".to_string()
            )
        );
        assert!(
            openapi_base_to_anthropic_partner_base(openapi, "google/gemini-2.5-flash").is_none(),
            "Gemini must stay on openapi Chat Completions"
        );
        assert!(
            openapi_base_to_anthropic_partner_base(
                "https://api.openai.com/v1",
                "anthropic/claude-mock-model"
            )
            .is_none()
        );
    }

    #[test]
    fn anthropic_partner_urls_normalize_stream_raw_predict() {
        let raw = "https://us-west1-aiplatform.googleapis.com/v1/projects/vertex-test-project/locations/us-west1/publishers/anthropic/models/claude-mock-model:rawPredict";
        assert_eq!(
            anthropic_partner_stream_url(raw),
            "https://us-west1-aiplatform.googleapis.com/v1/projects/vertex-test-project/locations/us-west1/publishers/anthropic/models/claude-mock-model:streamRawPredict"
        );
        assert_eq!(
            anthropic_partner_model_id(raw),
            Some("claude-mock-model".to_string())
        );
        let bare = "https://us-west1-aiplatform.googleapis.com/v1/projects/p-demo/locations/us-west1/publishers/anthropic/models/claude-mock-model";
        assert_eq!(
            anthropic_partner_stream_url(bare),
            format!("{bare}:streamRawPredict")
        );
    }

    #[test]
    fn parses_publisher_models_and_rewrites_ids() {
        let body = json!({
            "publisherModels": [
                {"name": "publishers/google/models/gemini-2.5-flash", "versionId": "gemini-2.5-flash"},
                {"name": "publishers/google/models/gemini-2.5-pro"},
                {"name": "publishers/google/models/textembedding-gecko"},
                {"name": ""},
            ]
        });
        let models = parse_publisher_models("google", &body);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["google/gemini-2.5-flash", "google/gemini-2.5-pro"]
        );
    }

    #[test]
    fn parses_anthropic_publisher_models() {
        let body = json!({
            "publisherModels": [
                {"name": "publishers/anthropic/models/claude-sonnet-4-5"},
                {"name": "publishers/anthropic/models/claude-opus-4"},
            ]
        });
        let models = parse_publisher_models("anthropic", &body);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "anthropic/claude-sonnet-4-5");
        assert_eq!(models[1].id, "anthropic/claude-opus-4");
    }

    #[test]
    fn empty_publisher_response_yields_no_models() {
        let body = json!({"publisherModels": []});
        assert!(parse_publisher_models("qwen", &body).is_empty());
        let body = json!({});
        assert!(parse_publisher_models("qwen", &body).is_empty());
    }

    #[test]
    fn openapi_endpoint_base_regional_and_global() {
        assert_eq!(
            openapi_endpoint_base("my-proj", "us-central1"),
            "https://us-central1-aiplatform.googleapis.com/v1/projects/my-proj/locations/us-central1/endpoints/openapi"
        );
        assert_eq!(
            openapi_endpoint_base("my-proj", "GLOBAL"),
            "https://aiplatform.googleapis.com/v1/projects/my-proj/locations/global/endpoints/openapi"
        );
    }
}
