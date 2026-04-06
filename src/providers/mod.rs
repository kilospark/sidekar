pub mod anthropic;
pub mod codex;
pub mod oauth;
pub mod openrouter;

use reqwest::{StatusCode, header::HeaderMap};
use serde::{Deserialize, Serialize};

static VERBOSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_verbose(v: bool) {
    VERBOSE.store(v, std::sync::atomic::Ordering::SeqCst);
}

pub fn is_verbose() -> bool {
    VERBOSE.load(std::sync::atomic::Ordering::SeqCst)
}

pub(super) fn log_api_request(url: &str, headers: &HeaderMap, body: &serde_json::Value) {
    if !is_verbose() {
        return;
    }

    crate::tunnel::tunnel_println("\x1b[2m--- API Request ---");
    crate::tunnel::tunnel_println(&format!("POST {url}"));
    crate::tunnel::tunnel_println(&format!("Headers: {headers:?}"));
    crate::tunnel::tunnel_println(&format!(
        "Body: {}",
        serde_json::to_string_pretty(body).unwrap_or_default()
    ));
    crate::tunnel::tunnel_println("---\x1b[0m");
}

pub(super) fn log_api_error(status: StatusCode, text: &str) {
    if !is_verbose() {
        return;
    }

    crate::tunnel::tunnel_println(&format!(
        "\x1b[2m--- API Error {status} ---\n{text}\n---\x1b[0m"
    ));
}

#[derive(Debug, Clone)]
pub(super) struct SseEvent {
    pub event_type: Option<String>,
    pub data: String,
}

pub(super) struct SseDecoder {
    buffer: String,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    pub fn push_chunk(&mut self, chunk: &[u8]) {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));

        if self.buffer.contains('\r') {
            self.buffer = self.buffer.replace("\r\n", "\n").replace('\r', "\n");
        }
    }

    pub fn next_event(&mut self) -> Option<SseEvent> {
        while let Some(event_end) = self.buffer.find("\n\n") {
            let event_text = self.buffer[..event_end].to_string();
            self.buffer = self.buffer[event_end + 2..].to_string();

            let mut event_type = None;
            let mut event_data_parts = Vec::new();

            for line in event_text.lines() {
                if let Some(rest) = line.strip_prefix("event: ") {
                    event_type = Some(rest.to_string());
                } else if let Some(rest) = line.strip_prefix("event:") {
                    event_type = Some(rest.trim().to_string());
                } else if let Some(rest) = line.strip_prefix("data: ") {
                    if rest == "[DONE]" {
                        continue;
                    }
                    event_data_parts.push(rest.to_string());
                } else if let Some(rest) = line.strip_prefix("data:") {
                    let rest = rest.trim_start();
                    if rest == "[DONE]" {
                        continue;
                    }
                    event_data_parts.push(rest.to_string());
                }
            }

            let data = event_data_parts.join("\n");
            if data.is_empty() {
                continue;
            }

            return Some(SseEvent { event_type, data });
        }

        None
    }
}

pub(super) fn parse_sse_json(event: &SseEvent) -> Option<serde_json::Value> {
    serde_json::from_str(&event.data).ok()
}

#[cfg(test)]
mod tests {
    use super::SseDecoder;

    #[test]
    fn sse_decoder_parses_named_events_with_crlf_chunks() {
        let mut decoder = SseDecoder::new();
        decoder.push_chunk(b"event: message_start\r\ndata: {\"ok\":1}\r\n\r\n");

        let event = decoder.next_event().expect("expected SSE event");
        assert_eq!(event.event_type.as_deref(), Some("message_start"));
        assert_eq!(event.data, "{\"ok\":1}");
    }

    #[test]
    fn sse_decoder_ignores_done_and_collects_data_only_events() {
        let mut decoder = SseDecoder::new();
        decoder.push_chunk(b"data: [DONE]\n\ndata: {\"type\":\"response.created\"}\n\n");

        let event = decoder.next_event().expect("expected SSE event");
        assert_eq!(event.event_type, None);
        assert_eq!(event.data, "{\"type\":\"response.created\"}");
        assert!(decoder.next_event().is_none());
    }
}

// ---------------------------------------------------------------------------
// Message types shared across providers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
    #[serde(rename = "tool_use")]
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    /// Local image bytes as base64 (e.g. from REPL paste of an image path). Serialized per provider.
    /// `source_path` is the on-disk location for one-turn-only handoff: after a successful turn we
    /// drop `data_base64` and keep path text so the model can re-read via tools.
    #[serde(rename = "image")]
    Image {
        media_type: String,
        data_base64: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_path: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
}

impl Usage {
    pub fn total_tokens(&self) -> u32 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StopReason {
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
}

// ---------------------------------------------------------------------------
// Tool ID sanitization for cross-provider session resumption
// ---------------------------------------------------------------------------

/// Sanitize a tool call ID for Anthropic (must match `^[a-zA-Z0-9_-]+$`).
/// Returns a borrowed reference when the input is already valid.
pub(crate) fn sanitize_id_anthropic(id: &str) -> std::borrow::Cow<'_, str> {
    if !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return std::borrow::Cow::Borrowed(id);
    }
    let sanitized: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    std::borrow::Cow::Owned(if sanitized.is_empty() {
        "id_0".to_string()
    } else {
        sanitized
    })
}

/// Sanitize a tool call ID for OpenAI-compatible APIs (must start with `call_`).
pub(crate) fn sanitize_id_openai(id: &str) -> String {
    // Strip pipe-separated item ID if present (Codex format)
    let base = id.split_once('|').map(|(call, _)| call).unwrap_or(id);
    let sanitized: String = base
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if sanitized.starts_with("call_") {
        sanitized
    } else {
        format!("call_{sanitized}")
    }
}

// ---------------------------------------------------------------------------
// Tool definitions sent to the LLM
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Streaming events emitted by providers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Emitted before each API call so the UI can show a waiting indicator.
    Waiting,
    /// Resolving model context limits (first turn only; may call the provider API).
    ResolvingContext,
    /// Opening the HTTP/SSE stream to the model (after the request is built).
    Connecting,
    /// Emitted before a tool executes so the UI can show progress.
    ToolExec {
        name: String,
        /// JSON object for the tool input (same as in the assistant message), for UI summaries.
        arguments_json: String,
    },
    /// Emitted when context compaction (LLM summarization) is in progress.
    Compacting,
    /// Emitted when a background activity ends without assistant output.
    Idle,
    TextDelta {
        delta: String,
    },
    ThinkingDelta {
        delta: String,
    },
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolCallDelta {
        index: usize,
        delta: String,
    },
    ToolCallEnd {
        index: usize,
    },
    Done {
        message: AssistantResponse,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantResponse {
    pub content: Vec<ContentBlock>,
    pub usage: Usage,
    pub stop_reason: StopReason,
    pub model: String,
}

/// Cached model metadata fetched from provider APIs.
static MODEL_CACHE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, (u32, u32)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Fetch context window for a model from the provider API.
/// Tries the provider's models endpoint first, falls back to static registry.
pub async fn fetch_context_window(model: &str, provider: &Provider) -> u32 {
    if let Some(&(ctx, _)) = MODEL_CACHE
        .lock()
        .ok()
        .and_then(|c| c.get(model).copied())
        .as_ref()
    {
        return ctx;
    }

    if let Some((ctx, max_out)) = fetch_model_limits(model, provider).await {
        if let Ok(mut cache) = MODEL_CACHE.lock() {
            cache.insert(model.to_string(), (ctx, max_out));
        }
        return ctx;
    }

    128_000 // safe default
}

/// Fetch max output tokens for a model from the provider API.
pub async fn fetch_max_output(model: &str, provider: &Provider) -> u32 {
    if let Some(&(_, max_out)) = MODEL_CACHE
        .lock()
        .ok()
        .and_then(|c| c.get(model).copied())
        .as_ref()
    {
        return max_out;
    }

    if let Some((ctx, max_out)) = fetch_model_limits(model, provider).await {
        if let Ok(mut cache) = MODEL_CACHE.lock() {
            cache.insert(model.to_string(), (ctx, max_out));
        }
        return max_out;
    }

    16_384 // safe default
}

/// Fetch (context_window, max_output) from the provider's models API.
async fn fetch_model_limits(model: &str, provider: &Provider) -> Option<(u32, u32)> {
    match provider {
        Provider::Anthropic { api_key, base_url } => {
            fetch_anthropic_model_limits(api_key, base_url, model).await
        }
        Provider::OpenRouter { api_key, base_url } => {
            fetch_openrouter_model_limits(api_key, base_url, model).await
        }
        Provider::Codex { .. } => None, // OpenAI /v1/models doesn't return context info
    }
}

/// Anthropic: GET /v1/models → max_input_tokens, max_tokens
async fn fetch_anthropic_model_limits(
    api_key: &str,
    base_url: &str,
    model: &str,
) -> Option<(u32, u32)> {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;

    let is_oauth = api_key.contains("sk-ant-oat");
    let mut req = client.get(&url).header("anthropic-version", "2023-06-01");
    if is_oauth {
        req = req.header("authorization", format!("Bearer {api_key}"));
    } else {
        req = req.header("x-api-key", api_key);
    }

    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }

    let body: serde_json::Value = resp.json().await.ok()?;
    let models = body.get("data").and_then(|d| d.as_array())?;

    for m in models {
        let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if id == model {
            let ctx = m
                .get("max_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(200_000) as u32;
            let max_out = m
                .get("max_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(16_000) as u32;
            return Some((ctx, max_out));
        }
    }

    None
}

/// OpenRouter: GET /v1/models → context_length, top_provider.max_completion_tokens
async fn fetch_openrouter_model_limits(
    api_key: &str,
    base_url: &str,
    model: &str,
) -> Option<(u32, u32)> {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;

    let resp = client
        .get(&url)
        .header("authorization", format!("Bearer {api_key}"))
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let body: serde_json::Value = resp.json().await.ok()?;
    let models = body.get("data").and_then(|d| d.as_array())?;

    for m in models {
        let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if id == model {
            let ctx = m
                .get("context_length")
                .and_then(|v| v.as_u64())
                .unwrap_or(128_000) as u32;
            let max_out = m
                .get("top_provider")
                .and_then(|tp| tp.get("max_completion_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(16_384) as u32;
            return Some((ctx, max_out));
        }
    }

    None
}

/// A model entry returned by a provider's list endpoint.
#[derive(Debug, Clone)]
pub struct RemoteModel {
    pub id: String,
    pub display_name: String,
    pub context_window: u32,
}

/// Fetch available models from a provider's API.
pub async fn fetch_model_list(provider_type: &str, api_key: &str) -> Vec<RemoteModel> {
    match provider_type {
        "anthropic" => fetch_anthropic_model_list(api_key).await,
        "codex" => fetch_codex_model_list(api_key).await,
        "openrouter" => fetch_openrouter_model_list(api_key).await,
        "opencode" => fetch_opencode_model_list(api_key).await,
        _ => Vec::new(),
    }
}

async fn fetch_anthropic_model_list(api_key: &str) -> Vec<RemoteModel> {
    let url = "https://api.anthropic.com/v1/models";
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let is_oauth = api_key.contains("sk-ant-oat");
    let mut req = client.get(url).header("anthropic-version", "2023-06-01");
    if is_oauth {
        // OAuth: use the /v1/models endpoint with bearer token + beta header
        req = req
            .header("authorization", format!("Bearer {api_key}"))
            .header("anthropic-beta", "oauth-2025-04-20");
    } else {
        req = req.header("x-api-key", api_key);
    }

    let resp = match req.send().await {
        Ok(r) => {
            if !r.status().is_success() {
                let status = r.status();
                let text = r.text().await.unwrap_or_default();
                eprintln!("\x1b[2m[models API {status}: {text}]\x1b[0m");
                return Vec::new();
            }
            r
        }
        Err(e) => {
            eprintln!("\x1b[2m[models API error: {e}]\x1b[0m");
            return Vec::new();
        }
    };

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let mut models = Vec::new();
    if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
        for m in data {
            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let name = m.get("display_name").and_then(|v| v.as_str()).unwrap_or(id);
            let ctx = m
                .get("max_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            if !id.is_empty() {
                models.push(RemoteModel {
                    id: id.to_string(),
                    display_name: name.to_string(),
                    context_window: ctx,
                });
            }
        }
    }
    if models.is_empty() {
        return Vec::new();
    }
    models
}

async fn fetch_codex_model_list(api_key: &str) -> Vec<RemoteModel> {
    let url = "https://chatgpt.com/backend-api/codex/models?client_version=1.0.0";
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let resp = match client
        .get(url)
        .header("authorization", format!("Bearer {api_key}"))
        .header("originator", "sidekar")
        .send()
        .await
    {
        Ok(r) => {
            if !r.status().is_success() {
                let status = r.status();
                let text = r.text().await.unwrap_or_default();
                eprintln!("\x1b[2m[models API {status}: {text}]\x1b[0m");
                return Vec::new();
            }
            r
        }
        Err(e) => {
            eprintln!("\x1b[2m[models API error: {e}]\x1b[0m");
            return Vec::new();
        }
    };

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let mut models = Vec::new();
    let arr = body
        .get("models")
        .and_then(|d| d.as_array())
        .or_else(|| body.get("data").and_then(|d| d.as_array()));
    if let Some(data) = arr {
        for m in data {
            let id = m
                .get("slug")
                .or_else(|| m.get("model"))
                .or_else(|| m.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let name = m.get("display_name").and_then(|v| v.as_str()).unwrap_or(id);
            let hidden = m.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false);
            if !id.is_empty() && !hidden {
                models.push(RemoteModel {
                    id: id.to_string(),
                    display_name: name.to_string(),
                    context_window: 0,
                });
            }
        }
    }
    models
}

async fn fetch_openrouter_model_list(api_key: &str) -> Vec<RemoteModel> {
    let url = "https://openrouter.ai/api/v1/models";
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let resp = match client
        .get(url)
        .header("authorization", format!("Bearer {api_key}"))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => return Vec::new(),
    };

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let mut models = Vec::new();
    if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
        for m in data {
            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let name = m.get("name").and_then(|v| v.as_str()).unwrap_or(id);
            let ctx = m
                .get("context_length")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            if !id.is_empty() {
                models.push(RemoteModel {
                    id: id.to_string(),
                    display_name: name.to_string(),
                    context_window: ctx,
                });
            }
        }
    }
    if models.is_empty() {
        return Vec::new();
    }
    models
}

async fn fetch_opencode_model_list(_api_key: &str) -> Vec<RemoteModel> {
    let url = "https://opencode.ai/zen/v1/models";
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let resp = match client
        .get(url)
        .header("anthropic-version", "2023-06-01")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => return Vec::new(),
    };

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let mut models = Vec::new();
    if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
        for m in data {
            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if !id.is_empty() {
                models.push(RemoteModel {
                    id: id.to_string(),
                    display_name: String::new(),
                    context_window: 0,
                });
            }
        }
    }
    models
}

// ---------------------------------------------------------------------------
// Provider — enum dispatch (no trait, 3 variants)
// ---------------------------------------------------------------------------

pub enum Provider {
    Anthropic {
        api_key: String,
        base_url: String,
    },
    #[allow(dead_code)]
    Codex {
        api_key: String,
        account_id: String,
        base_url: String,
    },
    OpenRouter {
        api_key: String,
        base_url: String,
    },
}

impl Provider {
    pub fn anthropic(api_key: String) -> Self {
        Provider::Anthropic {
            api_key,
            base_url: "https://api.anthropic.com".to_string(),
        }
    }

    pub fn codex(api_key: String, account_id: String) -> Self {
        Provider::Codex {
            api_key,
            account_id,
            base_url: "https://chatgpt.com/backend-api".to_string(),
        }
    }

    pub fn openrouter(api_key: String) -> Self {
        Provider::OpenRouter {
            api_key,
            base_url: "https://openrouter.ai/api".to_string(),
        }
    }

    /// OpenCode uses the Anthropic API shape with a different base URL.
    pub fn opencode(api_key: String) -> Self {
        Provider::Anthropic {
            api_key,
            base_url: "https://opencode.ai/zen".to_string(),
        }
    }

    pub fn api_key(&self) -> &str {
        match self {
            Provider::Anthropic { api_key, .. } => api_key,
            Provider::Codex { api_key, .. } => api_key,
            Provider::OpenRouter { api_key, .. } => api_key,
        }
    }

    pub fn provider_type(&self) -> &str {
        match self {
            Provider::Anthropic { base_url, .. } if base_url.contains("opencode.ai") => "opencode",
            Provider::Anthropic { .. } => "anthropic",
            Provider::Codex { .. } => "codex",
            Provider::OpenRouter { .. } => "openrouter",
        }
    }

    pub async fn stream(
        &self,
        model: &str,
        system_prompt: &str,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        prompt_cache_key: Option<&str>,
    ) -> anyhow::Result<tokio::sync::mpsc::UnboundedReceiver<StreamEvent>> {
        let max_retries = 3u32;
        let mut attempt = 0u32;

        loop {
            let result = self
                .stream_once(model, system_prompt, messages, tools, prompt_cache_key)
                .await;

            match &result {
                Err(e) if attempt < max_retries && is_retryable_error(e) => {
                    attempt += 1;
                    let delay = std::time::Duration::from_millis(500 * 2u64.pow(attempt - 1));
                    eprintln!("\x1b[33m[error: {e:#}]\x1b[0m");
                    eprintln!(
                        "\x1b[33m[retrying {attempt}/{max_retries} in {:.1}s...]\x1b[0m",
                        delay.as_secs_f32()
                    );
                    crate::broker::try_log_error(
                        "repl",
                        &format!("retrying ({attempt}/{max_retries})"),
                        Some(&format!("{e:#}")),
                    );
                    tokio::time::sleep(delay).await;
                }
                _ => return result,
            }
        }
    }

    async fn stream_once(
        &self,
        model: &str,
        system_prompt: &str,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        prompt_cache_key: Option<&str>,
    ) -> anyhow::Result<tokio::sync::mpsc::UnboundedReceiver<StreamEvent>> {
        match self {
            Provider::Anthropic { api_key, base_url } => {
                anthropic::stream(
                    api_key,
                    base_url,
                    model,
                    system_prompt,
                    messages,
                    tools,
                    prompt_cache_key,
                )
                .await
            }
            Provider::Codex {
                api_key,
                account_id,
                base_url,
            } => {
                codex::stream(
                    api_key,
                    account_id,
                    base_url,
                    model,
                    system_prompt,
                    messages,
                    tools,
                    prompt_cache_key,
                )
                .await
            }
            Provider::OpenRouter { api_key, base_url } => {
                openrouter::stream(
                    api_key,
                    base_url,
                    model,
                    system_prompt,
                    messages,
                    tools,
                    prompt_cache_key,
                )
                .await
            }
        }
    }
}

/// Check if an error is retryable (5xx, 429 rate limit, or connection failure).
fn is_retryable_error(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    // 5xx server errors
    if msg.contains("(500)")
        || msg.contains("(502)")
        || msg.contains("(503)")
        || msg.contains("(504)")
        || msg.contains("(529)")
    {
        return true;
    }
    // 429 rate limit
    if msg.contains("(429)") {
        return true;
    }
    // Connection failures
    if msg.contains("failed to connect")
        || msg.contains("connection reset")
        || msg.contains("timed out")
    {
        return true;
    }
    false
}
