//! Google Vertex AI OpenAI-compatible endpoint adapter.
//!
//! Vertex's OpenAI-compat surface lives at:
//!   `https://aiplatform.googleapis.com/v1/projects/<PROJECT>/locations/<LOC>/endpoints/openapi`
//!
//! Quirks vs. plain OpenAI-compat that this module handles centrally:
//!
//! 1. `/models` is **not** implemented at the openapi base (returns 404 HTML).
//!    The actual model catalog lives at
//!    `https://aiplatform.googleapis.com/v1beta1/publishers/<pub>/models`
//!    and must be queried per publisher with the
//!    `x-goog-user-project: <PROJECT>` header.
//!
//! 2. The chat endpoint requires model IDs in `<publisher>/<model-id>` form
//!    (e.g. `google/gemini-2.5-flash`). Bare `gemini-2.5-flash` returns 400.
//!    The publisher-models response uses `publishers/<pub>/models/<id>` —
//!    we transform to the `<pub>/<id>` form expected by the chat endpoint.
//!
//! 3. Auth is a gcloud OAuth access token (`ya29.*`) sent as
//!    `Authorization: Bearer <token>`. The `x-goog-user-project` header is
//!    required on the model-listing endpoint to attribute quota/billing.

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
                eprintln!("\x1b[33m[vertex: could not extract project from {base_url}]\x1b[0m");
            }
            return Vec::new();
        }
    };

    let client = match super::catalog_http_client(15) {
        Ok(c) => c,
        Err(e) => {
            if verbose {
                eprintln!("\x1b[33m[vertex: failed to build http client: {e}]\x1b[0m");
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
                        eprintln!("\x1b[33m[vertex: {url} failed: {e}]\x1b[0m");
                    }
                    return Vec::new();
                }
            };
            if !resp.status().is_success() {
                if verbose {
                    let status = resp.status();
                    eprintln!("\x1b[33m[vertex: {url} returned {status}]\x1b[0m");
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
        out.push(RemoteModel {
            id: chat_id,
            display_name: display,
            context_window: 0,
        });
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
                "https://aiplatform.googleapis.com/v1/projects/sf-internal-tooling/locations/global/endpoints/openapi"
            ),
            Some("sf-internal-tooling".to_string())
        );
        assert_eq!(
            extract_project(
                "https://aiplatform.googleapis.com/v1/projects/my-proj/locations/us-central1/endpoints/openapi/"
            ),
            Some("my-proj".to_string())
        );
        assert_eq!(extract_project("https://api.openai.com/v1"), None);
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
}
