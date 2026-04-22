pub mod anthropic;
pub mod codex;
pub mod gemini;
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

// ---------------------------------------------------------------------------
// Shared MITM proxy config for in-process streaming clients.
//
// Set once at startup (e.g. `sidekar repl --proxy`) and read by
// `build_streaming_client` below so per-turn traffic flows through sidekar's
// MITM proxy and gets captured in the `proxy_log` SQLite table — the same way
// PTY-wrapped agents are captured. Enables symmetric inspection of REPL vs
// PTY traffic without needing `--verbose` on either side.
// ---------------------------------------------------------------------------

pub struct SharedProxyConfig {
    pub port: u16,
    pub ca_pem: Vec<u8>,
}

static PROXY_CONFIG: std::sync::OnceLock<SharedProxyConfig> = std::sync::OnceLock::new();

pub fn set_shared_proxy(port: u16, ca_pem: Vec<u8>) {
    let _ = PROXY_CONFIG.set(SharedProxyConfig { port, ca_pem });
}

/// Build a reqwest client for streaming provider API calls. When
/// `set_shared_proxy` has been called (REPL `--proxy` path), the client is
/// configured to route through sidekar's MITM proxy and trust its ephemeral
/// CA. Otherwise returns a bare client with just the timeout, preserving
/// existing behavior for non-proxied runs.
pub(crate) fn build_streaming_client(
    timeout: std::time::Duration,
) -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().timeout(timeout);
    if let Some(cfg) = PROXY_CONFIG.get() {
        let proxy_url = format!("http://127.0.0.1:{}", cfg.port);
        builder = builder.proxy(reqwest::Proxy::all(&proxy_url)?);
        let cert = reqwest::Certificate::from_pem(&cfg.ca_pem)?;
        builder = builder.add_root_certificate(cert);
    }
    Ok(builder.build()?)
}

pub(crate) fn openai_chat_completions_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    }
}

fn openai_models_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if let Some(prefix) = base.strip_suffix("/chat/completions") {
        format!("{prefix}/models")
    } else if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    }
}

pub(super) fn log_api_request(url: &str, headers: &HeaderMap, body: &serde_json::Value) {
    if !is_verbose() {
        return;
    }

    let body_size = serde_json::to_string(body).map(|s| s.len()).unwrap_or(0);

    print_verbose_line("\x1b[2m--- API Request ---");
    print_verbose_line(&format!("POST {url}"));

    // Headers: one per line, redact auth values
    for (name, value) in headers.iter() {
        let val = value.to_str().unwrap_or("<binary>");
        let display = match name.as_str() {
            "authorization" | "x-api-key" => {
                if val.len() > 12 {
                    format!("{}...{}", &val[..8], &val[val.len() - 4..])
                } else {
                    "***".to_string()
                }
            }
            _ => val.to_string(),
        };
        print_verbose_line(&format!("  {name}: {display}"));
    }

    // Body summary: extract useful fields instead of dumping raw JSON
    let model = body.get("model").and_then(|v| v.as_str()).unwrap_or("?");
    let max_tokens = body
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .map(|n| n.to_string())
        .unwrap_or_else(|| "?".into());

    let system_summary = match body.get("system") {
        Some(serde_json::Value::Array(arr)) => {
            let total: usize = arr
                .iter()
                .map(|b| {
                    b.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.len())
                        .unwrap_or(0)
                })
                .sum();
            format!("{} blocks, {} bytes", arr.len(), total)
        }
        Some(serde_json::Value::String(s)) => format!("{} bytes", s.len()),
        _ => "none".into(),
    };

    let msg_count = body
        .get("messages")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let tool_count = body
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    print_verbose_line(&format!("  model: {model}"));
    print_verbose_line(&format!("  max_tokens: {max_tokens}"));
    print_verbose_line(&format!("  system: {system_summary}"));
    print_verbose_line(&format!("  messages: {msg_count}"));
    if tool_count > 0 {
        print_verbose_line(&format!("  tools: {tool_count}"));
    }
    print_verbose_line(&format!("  body size: {}", format_bytes(body_size)));

    // Full body with long strings truncated
    let compacted = compact_json(body);
    let json_str = serde_json::to_string_pretty(&compacted).unwrap_or_default();
    for line in json_str.lines() {
        print_verbose_line(&format!("  {line}"));
    }
    print_verbose_line("---\x1b[0m");
}

const MAX_STRING_LEN: usize = 256;

pub fn compact_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) if s.len() > MAX_STRING_LEN => {
            serde_json::Value::String(format!("<{} bytes>", s.len()))
        }
        serde_json::Value::Object(map) => {
            let mut new_map = serde_json::Map::new();
            for (k, v) in map {
                new_map.insert(k.clone(), compact_json(v));
            }
            serde_json::Value::Object(new_map)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(compact_json).collect())
        }
        _ => value.clone(),
    }
}

fn format_bytes(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
}

fn print_verbose_line(line: &str) {
    crate::tunnel::tunnel_println(line);
}

pub(super) fn log_api_error(status: StatusCode, text: &str) {
    if !is_verbose() {
        return;
    }

    print_verbose_line(&format!("\x1b[2m--- API Error {status} ---"));
    for line in text.lines() {
        print_verbose_line(line);
    }
    print_verbose_line("---\x1b[0m");
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
mod tests;

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
    /// Opaque encrypted reasoning blob returned by the Codex API when
    /// `include: ["reasoning.encrypted_content"]` is set.  Stored in
    /// assistant messages and replayed verbatim on subsequent turns so the
    /// server can reconstruct its reasoning chain without re-computing it,
    /// which enables prompt-cache hits across the full conversation history.
    #[serde(rename = "encrypted_reasoning")]
    EncryptedReasoning {
        /// Opaque base64-encoded blob from the server.
        encrypted_content: String,
        /// Human-readable summary entries (e.g. `[{"type":"summary_text","text":"..."}]`).
        summary: Vec<serde_json::Value>,
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

/// Sanitize a tool call ID for OpenAI-compat APIs (must start with `call_`).
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
// Per-provider stream tuning
//
// Providers differ in what knobs they expose (max_tokens, cache TTL, cache
// scope, etc.). `StreamConfig` collects the ones we actually vary and
// `Provider::default_stream_config` returns the defaults each provider's
// native CLI sends — which is what we observed in `proxy_log` captures.
// Keeping this as a struct (rather than constants sprinkled through each
// provider module) lets callers tweak per-turn without rewriting providers.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct StreamConfig {
    pub max_tokens: u32,
    /// Cache TTL sent on Anthropic `cache_control` markers. `None` uses the
    /// Anthropic default (5-minute ephemeral). `Some("1h")` enables the 1-hour
    /// cache — required to survive REPL pauses and match Claude Code behavior.
    pub cache_ttl: Option<String>,
    /// Cache scope sent on Anthropic SYSTEM-block `cache_control` markers
    /// only. `None` uses the default (request-scoped). `Some("global")`
    /// enables cross-request reuse — what Claude Code sends in its captured
    /// traffic. NOTE: Anthropic rejects `scope` on message-content markers
    /// (`messages.N.content.M.text.cache_control.ephemeral.scope: Extra
    /// inputs are not permitted`), so this field is applied ONLY to the
    /// system breakpoint, never to message breakpoints.
    pub cache_scope: Option<String>,
    /// Use WebSocket transport instead of HTTP POST + SSE. Currently only
    /// supported by the Codex provider (`wss://chatgpt.com/backend-api/…`).
    pub use_websocket: bool,
    /// Allow the model to emit multiple tool calls in a single response.
    /// Codex CLI always sends `true`.
    pub parallel_tool_calls: bool,
    /// Sampling temperature. Codex CLI sends `1.0` for reasoning models.
    pub temperature: Option<f32>,
    /// Reasoning configuration: `{ "effort": "high", "summary": "auto" }`.
    pub reasoning: Option<ReasoningConfig>,
    /// Text generation configuration: `{ "verbosity": "verbose" }`.
    pub text_verbosity: Option<String>,
}

/// Reasoning parameters sent to the Codex Responses API.
#[derive(Debug, Clone)]
pub struct ReasoningConfig {
    pub effort: String,
    pub summary: String,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            max_tokens: 16_000,
            cache_ttl: None,
            cache_scope: None,
            use_websocket: false,
            parallel_tool_calls: false,
            temperature: None,
            reasoning: None,
            text_verbosity: None,
        }
    }
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
    /// Provider-specific response identifier for stateful chaining (e.g. codex
    /// `previous_response_id`). Empty for providers that don't support it.
    #[serde(default)]
    pub response_id: String,
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
        Provider::Anthropic {
            api_key, base_url, ..
        } => fetch_anthropic_model_limits(api_key, base_url, model).await,
        Provider::OpenRouter {
            api_key, base_url, ..
        } => fetch_openrouter_model_limits(api_key, base_url, model).await,
        Provider::OpenAiCompat {
            api_key, base_url, ..
        } => fetch_openai_compat_model_limits(api_key, base_url, model).await,
        Provider::Codex { .. } => None, // OpenAI /v1/models doesn't return context info
        Provider::Gemini {
            api_key, base_url, ..
        } => gemini::fetch_gemini_model_limits(api_key, base_url, model).await,
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
    let url = openai_models_url(base_url);
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

/// Generic OpenAI-compat: GET /v1/models, using common context fields if exposed.
async fn fetch_openai_compat_model_limits(
    api_key: &str,
    base_url: &str,
    model: &str,
) -> Option<(u32, u32)> {
    let url = openai_models_url(base_url);
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
                .or_else(|| m.get("context_window"))
                .or_else(|| m.get("max_context_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(128_000) as u32;
            let max_out = m
                .get("max_output_tokens")
                .or_else(|| m.get("max_completion_tokens"))
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
        "grok" => fetch_openai_compat_model_list(api_key, oauth::GROK_BASE_URL).await,
        "opencode" => fetch_opencode_model_list(api_key).await,
        "gemini" => gemini::fetch_gemini_model_list(api_key).await,
        _ => Vec::new(),
    }
}

pub async fn fetch_model_list_for_provider(provider: &Provider) -> Vec<RemoteModel> {
    match provider {
        Provider::Anthropic {
            api_key, base_url, ..
        } if base_url.contains("opencode.ai") => fetch_opencode_model_list(api_key).await,
        Provider::Anthropic { api_key, .. } => fetch_anthropic_model_list(api_key).await,
        Provider::Codex { api_key, .. } => fetch_codex_model_list(api_key).await,
        Provider::OpenRouter { api_key, .. } => fetch_openrouter_model_list(api_key).await,
        Provider::OpenAiCompat {
            api_key, base_url, ..
        } => fetch_openai_compat_model_list(api_key, base_url).await,
        Provider::Gemini { api_key, .. } => gemini::fetch_gemini_model_list(api_key).await,
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
                crate::broker::try_log_error(
                    "models",
                    &format!("API {status}"),
                    Some(&text),
                );
                return Vec::new();
            }
            r
        }
        Err(e) => {
            crate::broker::try_log_error("models", "API error", Some(&format!("{e:#}")));
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
                crate::broker::try_log_error(
                    "models",
                    &format!("API {status}"),
                    Some(&text),
                );
                return Vec::new();
            }
            r
        }
        Err(e) => {
            crate::broker::try_log_error("models", "API error", Some(&format!("{e:#}")));
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
    fetch_openai_compat_model_list(api_key, "https://openrouter.ai/api").await
}

pub async fn fetch_openai_compat_model_list(api_key: &str, base_url: &str) -> Vec<RemoteModel> {
    let url = openai_models_url(base_url);
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
            let name = m
                .get("name")
                .or_else(|| m.get("display_name"))
                .and_then(|v| v.as_str())
                .unwrap_or(id);
            let ctx = m
                .get("context_length")
                .or_else(|| m.get("context_window"))
                .or_else(|| m.get("max_context_tokens"))
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
        /// Stored credential nickname — enables auto-refresh on 401.
        credential: Option<String>,
    },
    #[allow(dead_code)]
    Codex {
        api_key: String,
        account_id: String,
        base_url: String,
        credential: Option<String>,
    },
    OpenRouter {
        api_key: String,
        base_url: String,
        credential: Option<String>,
    },
    OpenAiCompat {
        api_key: String,
        base_url: String,
        provider_type: String,
        display_name: String,
        credential: Option<String>,
    },
    /// Google Gemini via native `generativelanguage.googleapis.com`
    /// API. Static API key auth (header `x-goog-api-key`); no OAuth
    /// refresh flow. Supports `cachedContents` for multi-turn token
    /// savings (wiring in a follow-up commit).
    Gemini {
        api_key: String,
        base_url: String,
        credential: Option<String>,
    },
}

impl Provider {
    pub fn anthropic(api_key: String, credential: Option<String>) -> Self {
        Provider::Anthropic {
            api_key,
            base_url: "https://api.anthropic.com".to_string(),
            credential,
        }
    }

    pub fn codex(api_key: String, account_id: String, credential: Option<String>) -> Self {
        Provider::Codex {
            api_key,
            account_id,
            base_url: "https://chatgpt.com/backend-api".to_string(),
            credential,
        }
    }

    pub fn openrouter(api_key: String, credential: Option<String>) -> Self {
        Provider::OpenRouter {
            api_key,
            base_url: "https://openrouter.ai/api".to_string(),
            credential,
        }
    }

    pub fn grok(api_key: String, credential: Option<String>) -> Self {
        Provider::OpenAiCompat {
            api_key,
            base_url: oauth::GROK_BASE_URL.to_string(),
            provider_type: "grok".to_string(),
            display_name: "Grok".to_string(),
            credential,
        }
    }

    pub fn openai_compat(
        api_key: String,
        base_url: String,
        display_name: String,
        credential: Option<String>,
    ) -> Self {
        Provider::OpenAiCompat {
            api_key,
            base_url,
            provider_type: "oac".to_string(),
            display_name,
            credential,
        }
    }

    /// OpenCode uses the Anthropic API shape with a different base URL.
    pub fn opencode(api_key: String, credential: Option<String>) -> Self {
        Provider::Anthropic {
            api_key,
            base_url: "https://opencode.ai/zen".to_string(),
            credential,
        }
    }

    /// Gemini native provider. Uses Google's generativelanguage v1beta
    /// API directly (not the OpenAI-compat shim), giving access to
    /// thinking tokens, cachedContents, and native usageMetadata.
    pub fn gemini(api_key: String, credential: Option<String>) -> Self {
        Provider::Gemini {
            api_key,
            base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
            credential,
        }
    }

    pub fn api_key(&self) -> &str {
        match self {
            Provider::Anthropic { api_key, .. } => api_key,
            Provider::Codex { api_key, .. } => api_key,
            Provider::OpenRouter { api_key, .. } => api_key,
            Provider::OpenAiCompat { api_key, .. } => api_key,
            Provider::Gemini { api_key, .. } => api_key,
        }
    }

    pub fn credential(&self) -> Option<&str> {
        match self {
            Provider::Anthropic { credential, .. } => credential.as_deref(),
            Provider::Codex { credential, .. } => credential.as_deref(),
            Provider::OpenRouter { credential, .. } => credential.as_deref(),
            Provider::OpenAiCompat { credential, .. } => credential.as_deref(),
            Provider::Gemini { credential, .. } => credential.as_deref(),
        }
    }

    pub fn provider_type(&self) -> &str {
        match self {
            Provider::Anthropic { base_url, .. } if base_url.contains("opencode.ai") => "opencode",
            Provider::Anthropic { .. } => "anthropic",
            Provider::Codex { .. } => "codex",
            Provider::OpenRouter { .. } => "openrouter",
            Provider::OpenAiCompat { provider_type, .. } => provider_type,
            Provider::Gemini { .. } => "gemini",
        }
    }

    /// Per-provider defaults matching the native CLI behavior (observed in
    /// `proxy_log` captures). Anthropic gets Claude Code's `max_tokens=64k`
    /// plus a 1-hour cache TTL and `scope: "global"` so REPL traffic actually
    /// hits the prompt cache instead of the 5-minute ephemeral default — the
    /// single biggest driver of per-turn token drain on multi-turn chats.
    ///
    /// `scope: "global"` is only applied to SYSTEM breakpoints. Anthropic
    /// rejects it on message-content breakpoints (`messages.N.content.M.text.
    /// cache_control.ephemeral.scope: Extra inputs are not permitted`) but
    /// accepts it on system blocks — which is exactly how Claude Code's
    /// captured traffic uses it. The scope flag is what lets the cache survive
    /// the volatile per-request billing header (block 0, which changes every
    /// turn because `cch` is a body hash).
    pub fn default_stream_config(&self) -> StreamConfig {
        match self {
            Provider::Anthropic { .. } => StreamConfig {
                max_tokens: 64_000,
                cache_ttl: Some("1h".into()),
                cache_scope: Some("global".into()),
                ..StreamConfig::default()
            },
            Provider::Codex { .. } => StreamConfig {
                use_websocket: true,
                // NOTE: parallel_tool_calls, reasoning, and text_verbosity
                // are disabled for now — adding them correlated with 0% cache
                // hits vs ~48% without them.  Re-enable one at a time to
                // isolate the regression.
                ..StreamConfig::default()
            },
            Provider::OpenRouter { .. } => StreamConfig::default(),
            Provider::OpenAiCompat { .. } => StreamConfig::default(),
            // Gemini defaults: plain StreamConfig is fine for commit 1
            // (no caching, no thinking-budget, no reasoning). Commit 2
            // adds gemini_caching / gemini_cache_ttl_secs /
            // gemini_cache_min_tokens via new StreamConfig fields;
            // their defaults will be populated here.
            Provider::Gemini { .. } => StreamConfig::default(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn stream(
        &self,
        model: &str,
        system_prompt: &str,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        prompt_cache_key: Option<&str>,
        previous_response_id: Option<&str>,
        cached_ws: Option<codex::CachedWs>,
    ) -> anyhow::Result<(
        tokio::sync::mpsc::UnboundedReceiver<StreamEvent>,
        tokio::sync::oneshot::Receiver<Option<codex::CachedWs>>,
    )> {
        let max_retries = 3u32;
        let mut attempt = 0u32;
        let mut ws = cached_ws;
        let mut refreshed_key: Option<String> = None;
        let mut auth_retry_used = false;

        loop {
            let result = self
                .stream_once(
                    model,
                    system_prompt,
                    messages,
                    tools,
                    prompt_cache_key,
                    previous_response_id,
                    ws.take(),
                    refreshed_key.as_deref(),
                )
                .await;

            match &result {
                // Token expired server-side (or our expires_at drifted). Force
                // a refresh via the stored refresh_token and retry once.
                Err(e) if !auth_retry_used && is_auth_error(e) && self.credential().is_some() => {
                    auth_retry_used = true;
                    let cred = self.credential().expect("checked above").to_string();
                    match oauth::force_refresh_token(&cred).await {
                        Ok(new_key) => {
                            refreshed_key = Some(new_key);
                            eprintln!("\x1b[2m[credential `{cred}` refreshed after 401, retrying]\x1b[0m");
                        }
                        Err(refresh_err) => {
                            eprintln!(
                                "\x1b[31m[credential `{cred}` refresh failed: {refresh_err:#}]\x1b[0m"
                            );
                            eprintln!(
                                "\x1b[33m[run `/credential {cred}` to re-resolve, or `sidekar repl login {cred}` to re-authenticate]\x1b[0m"
                            );
                            return result;
                        }
                    }
                }
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

    #[allow(clippy::too_many_arguments)]
    async fn stream_once(
        &self,
        model: &str,
        system_prompt: &str,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        prompt_cache_key: Option<&str>,
        previous_response_id: Option<&str>,
        cached_ws: Option<codex::CachedWs>,
        api_key_override: Option<&str>,
    ) -> anyhow::Result<(
        tokio::sync::mpsc::UnboundedReceiver<StreamEvent>,
        tokio::sync::oneshot::Receiver<Option<codex::CachedWs>>,
    )> {
        let stream_config = self.default_stream_config();
        match self {
            Provider::Anthropic {
                api_key, base_url, ..
            } => {
                let key = api_key_override.unwrap_or(api_key);
                let rx = anthropic::stream(
                    key,
                    base_url,
                    model,
                    system_prompt,
                    messages,
                    tools,
                    prompt_cache_key,
                    &stream_config,
                )
                .await?;
                Ok(no_ws_reclaim(rx))
            }
            Provider::Codex {
                api_key,
                account_id,
                base_url,
                ..
            } => {
                let key = api_key_override.unwrap_or(api_key);
                if stream_config.use_websocket {
                    codex::stream_ws(
                        key,
                        account_id,
                        base_url,
                        model,
                        system_prompt,
                        messages,
                        tools,
                        prompt_cache_key,
                        previous_response_id,
                        &stream_config,
                        cached_ws,
                    )
                    .await
                } else {
                    let rx = codex::stream(
                        key,
                        account_id,
                        base_url,
                        model,
                        system_prompt,
                        messages,
                        tools,
                        prompt_cache_key,
                        previous_response_id,
                        &stream_config,
                    )
                    .await?;
                    Ok(no_ws_reclaim(rx))
                }
            }
            Provider::OpenRouter {
                api_key, base_url, ..
            } => {
                let key = api_key_override.unwrap_or(api_key);
                let rx = openrouter::stream(
                    key,
                    base_url,
                    model,
                    system_prompt,
                    messages,
                    tools,
                    prompt_cache_key,
                )
                .await?;
                Ok(no_ws_reclaim(rx))
            }
            Provider::OpenAiCompat {
                api_key,
                base_url,
                display_name,
                ..
            } => {
                let key = api_key_override.unwrap_or(api_key);
                let rx = openrouter::stream_with_provider(
                    display_name,
                    key,
                    base_url,
                    model,
                    system_prompt,
                    messages,
                    tools,
                    prompt_cache_key,
                )
                .await?;
                Ok(no_ws_reclaim(rx))
            }
            Provider::Gemini {
                api_key, base_url, ..
            } => {
                let key = api_key_override.unwrap_or(api_key);
                let rx = gemini::stream(
                    key,
                    base_url,
                    model,
                    system_prompt,
                    messages,
                    tools,
                    prompt_cache_key,
                )
                .await?;
                Ok(no_ws_reclaim(rx))
            }
        }
    }
}

/// Wrap a plain event receiver with a pre-resolved "no WS to reclaim" oneshot
/// for providers that don't use persistent WebSocket connections.
fn no_ws_reclaim(
    rx: tokio::sync::mpsc::UnboundedReceiver<StreamEvent>,
) -> (
    tokio::sync::mpsc::UnboundedReceiver<StreamEvent>,
    tokio::sync::oneshot::Receiver<Option<codex::CachedWs>>,
) {
    let (tx, reclaim_rx) = tokio::sync::oneshot::channel();
    let _ = tx.send(None);
    (rx, reclaim_rx)
}

/// Check if an error looks like an auth failure (401 / authentication_error).
/// Matches the message shape emitted by `anthropic::stream` and peers after a
/// non-success status.
fn is_auth_error(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    msg.contains("(401") || msg.contains("authentication_error") || msg.contains("Unauthorized")
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
