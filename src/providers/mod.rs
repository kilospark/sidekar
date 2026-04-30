pub mod anthropic;
pub mod codex;
pub mod gemini;
pub mod oauth;
pub mod openrouter;
pub mod session_lock;
pub mod vertex;

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
    } else if url_has_path(base) {
        // Custom endpoint with existing path (e.g. Vertex AI) — append directly, no /v1/
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    }
}

fn openai_models_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if let Some(prefix) = base.strip_suffix("/chat/completions") {
        format!("{prefix}/models")
    } else if base.ends_with("/v1") || url_has_path(base) {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    }
}

/// Returns true if the URL has a non-trivial path (more than just the host).
/// e.g. "https://api.x.ai" → false, "https://aiplatform.googleapis.com/v1/projects/foo" → true
fn url_has_path(url: &str) -> bool {
    // Strip scheme
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    // If there's a '/' after the host, there's a path
    after_scheme.contains('/')
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
            // Sensitive auth headers. Keep a tiny prefix+suffix so
            // request logs remain distinguishable across retries
            // without ever printing the full secret.
            "authorization" | "x-api-key" | "x-goog-api-key" | "api-key" => {
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

/// Streaming SSE frame decoder.
///
/// Holds an append-only buffer and a read cursor rather than reslicing
/// the buffer after every event. Previously `next_event` did
/// `self.buffer = self.buffer[end..].to_string()` per event, which is
/// O(remaining) each call. Over a long stream with thousands of small
/// `data: {...}` frames the cumulative cost is O(n²) in the number of
/// frames times the response tail — a real CPU cost, especially on
/// fast streams (Anthropic/Gemini routinely emit 50-80 deltas/sec
/// during prose). Amortization strategy: advance a `read_pos` cursor,
/// and only physically drain the consumed prefix when it grows large
/// enough that carrying it is wasteful (>8KB or >half the buffer).
/// `drain(..read_pos)` is O(remaining) once per compaction, not per
/// event, giving amortized O(1) per event.
pub(super) struct SseDecoder {
    /// Raw accumulated bytes, append-only until `compact()` trims the
    /// consumed prefix. Always valid UTF-8 (push_chunk runs incoming
    /// bytes through from_utf8_lossy first).
    buffer: String,
    /// Byte offset within `buffer` of the next unread byte. Events
    /// between 0..read_pos have already been returned. Monotonically
    /// advances until `compact()` resets it to 0 by draining the
    /// consumed prefix.
    read_pos: usize,
}

/// Drop the consumed prefix once it passes this size. 8KB is large
/// enough that we aren't compacting on every frame, small enough that
/// RSS doesn't balloon on a long streamed response. Tuned against
/// Anthropic's ~200-byte average frame size: ~40 events between
/// compactions in typical prose streaming.
const SSE_COMPACT_THRESHOLD: usize = 8 * 1024;

impl SseDecoder {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            read_pos: 0,
        }
    }

    pub fn push_chunk(&mut self, chunk: &[u8]) {
        // Normalize CRLF → LF as bytes come in. The old impl rescanned
        // the entire buffer on every chunk (`buffer.replace(...)`) when
        // a stray \r was present — quadratic if \r ever stuck around.
        // Here we only transform the incoming slice, never the already-
        // accumulated tail.
        let decoded = String::from_utf8_lossy(chunk);
        if decoded.contains('\r') {
            // Small local normalization: replace CRLF and lone CR with
            // LF only within this chunk. Cross-chunk split like "..\r"
            // followed by "\n.." is handled naturally since only the
            // trailing \r becomes a lone \n (SSE treats either as a
            // line separator per spec).
            let normalized = decoded.replace("\r\n", "\n").replace('\r', "\n");
            self.buffer.push_str(&normalized);
        } else {
            self.buffer.push_str(&decoded);
        }
    }

    pub fn next_event(&mut self) -> Option<SseEvent> {
        loop {
            // Search from the read cursor, not from 0. `find` returns
            // an offset relative to the search slice, so add read_pos
            // to make it an absolute position in the buffer.
            let unread = &self.buffer[self.read_pos..];
            let rel_end = unread.find("\n\n")?;
            let event_end = self.read_pos + rel_end;

            // Parse the event. We can't hold &str borrows into
            // self.buffer across self.maybe_compact() below (that
            // takes &mut self), so materialize event_type and data
            // into owned Strings first — one allocation per field
            // that's actually returned, which we'd have paid on the
            // old code path anyway.
            let event_text = &self.buffer[self.read_pos..event_end];
            let mut event_type: Option<String> = None;
            let mut data = String::new();

            for line in event_text.lines() {
                if let Some(rest) = line.strip_prefix("event: ") {
                    event_type = Some(rest.to_string());
                } else if let Some(rest) = line.strip_prefix("event:") {
                    event_type = Some(rest.trim().to_string());
                } else if let Some(rest) = line
                    .strip_prefix("data: ")
                    .or_else(|| line.strip_prefix("data:").map(str::trim_start))
                {
                    if rest == "[DONE]" {
                        continue;
                    }
                    // SSE multi-line data: concatenate with "\n"
                    // between lines (per spec, single field).
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(rest);
                }
            }

            // Advance the cursor past this event (including the
            // trailing "\n\n" separator) before returning so the
            // caller's next call resumes correctly. Also runs in the
            // `continue` path so empty/DONE frames don't stall the
            // cursor.
            self.read_pos = event_end + 2;
            self.maybe_compact();

            if data.is_empty() {
                continue;
            }
            return Some(SseEvent { event_type, data });
        }
    }

    /// Drop the consumed prefix when it's grown large enough to be
    /// worth a single memmove. Keeping the prefix indefinitely would
    /// retain the entire response text in memory even after every
    /// event has been returned — harmless within one stream (the
    /// decoder dies at stream end), but wasteful during long
    /// responses. Drain amortizes to O(1) per event when called from
    /// `next_event` because a drain only happens once per ~40 events
    /// at typical frame sizes.
    fn maybe_compact(&mut self) {
        if self.read_pos >= SSE_COMPACT_THRESHOLD
            || (self.read_pos > 0 && self.read_pos * 2 >= self.buffer.len())
        {
            self.buffer.drain(..self.read_pos);
            self.read_pos = 0;
        }
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
        /// Gemini thought-signature for this function-call part.
        /// Must be replayed verbatim on the next request so the API
        /// can validate the call was model-generated.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
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
    /// Plain-text reasoning trace returned by OpenAI-compat reasoning models
    /// (DeepSeek `reasoning_content`, Kimi `reasoning`). Captured during a
    /// streamed assistant turn and replayed verbatim on the next request so
    /// the upstream provider accepts the subsequent tool_result. Stored as a
    /// sibling of `Text`/`ToolCall` blocks on the same assistant message.
    #[serde(rename = "reasoning")]
    Reasoning { text: String },
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

/// Snapshot of provider rate-limit / quota state from response headers (or 429 body).
/// All fields optional — providers vary in coverage.
/// `reset_at` and `session_reset_at` are Unix epoch seconds.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RateLimitSnapshot {
    /// Requests-per-window remaining (e.g. RPM bucket).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_remaining: Option<u64>,
    /// Requests-per-window limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_limit: Option<u64>,
    /// Tokens-per-window remaining (combined or input — provider-dependent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_remaining: Option<u64>,
    /// Tokens-per-window limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_limit: Option<u64>,
    /// Earliest header-reset (Unix epoch seconds) — short-window throttle (e.g. 60s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<u64>,
    /// Anthropic Pro/Team session-window reset (Unix epoch seconds), parsed from 429 body
    /// when message contains "Your limit will reset at HH:MM ...". Multi-hour scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_reset_at: Option<u64>,
    /// Anthropic unified-5h utilization percent (0..=100), if returned via OAuth headers.
    pub util_5h_pct: Option<u32>,
    pub reset_5h_at: Option<u64>,
    /// Anthropic unified-7d utilization percent (0..=100).
    pub util_7d_pct: Option<u32>,
    pub reset_7d_at: Option<u64>,
}

impl RateLimitSnapshot {
    /// Returns Self wrapped in Option::Some, or None if all fields are empty.
    pub fn into_option(self) -> Option<Self> {
        if self.is_empty() { None } else { Some(self) }
    }

    /// True if no fields populated — used to skip emitting empty snapshots.
    pub fn is_empty(&self) -> bool {
        self.requests_remaining.is_none()
            && self.requests_limit.is_none()
            && self.tokens_remaining.is_none()
            && self.tokens_limit.is_none()
            && self.reset_at.is_none()
            && self.session_reset_at.is_none()
            && self.util_5h_pct.is_none()
            && self.util_7d_pct.is_none()
    }

    /// Parse rate-limit headers in the OpenAI-style schema (`x-ratelimit-*`).
    /// Used by OpenAI, Codex, OpenRouter, xAI, most OpenAI-compat hosts.
    pub fn from_openai_headers(h: &reqwest::header::HeaderMap) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let r1 = parse_reset_header(h, "x-ratelimit-reset-requests", now);
        let r2 = parse_reset_header(h, "x-ratelimit-reset-tokens", now);
        // OpenRouter sometimes uses bare "x-ratelimit-reset" (ms epoch).
        let r3 = parse_reset_header(h, "x-ratelimit-reset", now);
        Self {
            requests_limit: parse_u64_header(h, "x-ratelimit-limit-requests"),
            requests_remaining: parse_u64_header(h, "x-ratelimit-remaining-requests"),
            tokens_limit: parse_u64_header(h, "x-ratelimit-limit-tokens"),
            tokens_remaining: parse_u64_header(h, "x-ratelimit-remaining-tokens"),
            reset_at: [r1, r2, r3].into_iter().flatten().min(),
            ..Self::default()
        }
    }

    /// Parse rate-limit headers in Anthropic's schema (`anthropic-ratelimit-*`).
    pub fn from_anthropic_headers(h: &reqwest::header::HeaderMap) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let resets = [
            parse_reset_header(h, "anthropic-ratelimit-requests-reset", now),
            parse_reset_header(h, "anthropic-ratelimit-tokens-reset", now),
            parse_reset_header(h, "anthropic-ratelimit-input-tokens-reset", now),
            parse_reset_header(h, "anthropic-ratelimit-output-tokens-reset", now),
        ];
        Self {
            requests_limit: parse_u64_header(h, "anthropic-ratelimit-requests-limit"),
            requests_remaining: parse_u64_header(h, "anthropic-ratelimit-requests-remaining"),
            // Prefer combined tokens bucket when present.
            tokens_limit: parse_u64_header(h, "anthropic-ratelimit-tokens-limit")
                .or_else(|| parse_u64_header(h, "anthropic-ratelimit-input-tokens-limit")),
            tokens_remaining: parse_u64_header(h, "anthropic-ratelimit-tokens-remaining")
                .or_else(|| parse_u64_header(h, "anthropic-ratelimit-input-tokens-remaining")),
            reset_at: resets.into_iter().flatten().min(),
            util_5h_pct: h
                .get("anthropic-ratelimit-unified-5h-utilization")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<f64>().ok())
                .map(|f| (f * 100.0).round().clamp(0.0, 100.0) as u32),
            reset_5h_at: parse_u64_header(h, "anthropic-ratelimit-unified-5h-reset"),
            util_7d_pct: h
                .get("anthropic-ratelimit-unified-7d-utilization")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<f64>().ok())
                .map(|f| (f * 100.0).round().clamp(0.0, 100.0) as u32),
            reset_7d_at: parse_u64_header(h, "anthropic-ratelimit-unified-7d-reset"),
            ..Self::default()
        }
    }

    /// Parse Anthropic's Pro/Team session-window reset from a 429 error body.
    /// Body shape: { error: { type: "rate_limit_error", message: "... reset at 14:32 UTC ..." } }
    /// Sets `session_reset_at` if a HH:MM reset time is found.
    pub fn parse_anthropic_session_reset(body: &str) -> Option<u64> {
        // Look for "reset at HH:MM" or "resets at HH:MM" (UTC implied).
        let re = regex::Regex::new(r"reset(?:s)? at (\d{1,2}):(\d{2})").ok()?;
        let caps = re.captures(body)?;
        let hour: u32 = caps.get(1)?.as_str().parse().ok()?;
        let minute: u32 = caps.get(2)?.as_str().parse().ok()?;
        if hour > 23 || minute > 59 {
            return None;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        // Compute target time today (UTC).
        let secs_today = now % 86_400;
        let target_secs = (hour as u64) * 3600 + (minute as u64) * 60;
        let day_start = now - secs_today;
        let mut target = day_start + target_secs;
        if target <= now {
            target += 86_400; // next day
        }
        Some(target)
    }
}

fn parse_u64_header(h: &reqwest::header::HeaderMap, name: &str) -> Option<u64> {
    h.get(name)?.to_str().ok()?.trim().parse::<u64>().ok()
}

/// Parse a rate-limit reset header. Accepts:
///   - Unix epoch seconds (10-digit) → returned as-is
///   - Unix epoch milliseconds (13-digit) → divided by 1000
///   - ISO 8601 timestamp → converted (best-effort; falls through if no parser)
///   - Duration like "60s", "1.5s", "60" → added to `now`
fn parse_reset_header(h: &reqwest::header::HeaderMap, name: &str, now: u64) -> Option<u64> {
    let raw = h.get(name)?.to_str().ok()?.trim();
    if let Ok(n) = raw.parse::<u64>() {
        // 13-digit → ms epoch
        if n > 1_000_000_000_000 {
            return Some(n / 1000);
        }
        // 10-digit → s epoch (treat as absolute if plausibly in the future)
        if n > 1_000_000_000 {
            return Some(n);
        }
        // small int → seconds-from-now
        return Some(now + n);
    }
    // Try float (e.g. "1.5s")
    let stripped = raw.trim_end_matches('s').trim_end_matches("ms");
    if let Ok(f) = stripped.parse::<f64>() {
        let secs = if raw.ends_with("ms") { f / 1000.0 } else { f };
        return Some(now + secs.round() as u64);
    }
    None
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
    /// Gemini: enable implicit `cachedContents` lifecycle for stable
    /// prefixes. When true, the Gemini adapter fingerprints
    /// (system + tools + history-prefix) per turn and reuses or
    /// creates a cached content object so the server bills that
    /// prefix at the cache-read rate (~75% discount). Defaults to
    /// true for Provider::Gemini; ignored by other providers.
    pub gemini_caching: bool,
    /// Gemini: cache TTL in seconds. 3600 (1h) matches our
    /// Anthropic default. Shorter TTLs reduce storage cost for
    /// sessions that rarely return; longer TTLs are pointless since
    /// the fingerprint must match exactly for reuse.
    pub gemini_cache_ttl_secs: u32,
    /// Gemini: minimum prefix token count (estimated) before
    /// attempting to create a cache. Gemini 2.5 Flash requires
    /// 4096; 2.5 Pro requires 1024. Default 4096 is safe for both.
    /// Below this, the creation round-trip costs more than the
    /// cache-read savings for a single reuse.
    pub gemini_cache_min_tokens: u32,
    /// KV key under which credentials are stored (e.g. "oauth.claude-a").
    /// Used to persist session-window lockouts on 429.
    pub credential_kv_key: Option<String>,
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
            // Gemini-specific fields default to "off" for non-Gemini
            // providers. Provider::Gemini overrides via
            // default_stream_config.
            gemini_caching: false,
            gemini_cache_ttl_secs: 3600,
            gemini_cache_min_tokens: 4096,
            credential_kv_key: None,
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
    /// Quota / rate-limit snapshot parsed from response headers (or 429 body).
    /// `None` for providers that don't expose any (Gemini, Vertex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitSnapshot>,
}

/// Cached model metadata fetched from provider APIs.
static MODEL_CACHE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, (u32, u32)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Non-blocking, cache-only context window lookup for display
/// surfaces like `/status`.
///
/// Returns `Some(context_window)` iff the model has already been
/// queried via `fetch_context_window` during this process (i.e. the
/// agent has already run at least one turn on this model). Returns
/// `None` otherwise — callers must then decide whether to block on
/// `fetch_context_window` or display an "unknown" placeholder.
///
/// Exists separately from `fetch_context_window` because the REPL's
/// slash-command dispatcher is synchronous and must not block on a
/// network round-trip to render `/status`. By the time the user can
/// run `/status` on a model, the first turn has almost always
/// populated the cache anyway.
pub fn cached_context_window(model: &str) -> Option<u32> {
    MODEL_CACHE
        .lock()
        .ok()
        .and_then(|c| c.get(model).map(|(ctx, _)| *ctx))
}

/// Cache hit → Some; cache miss → network fetch, populate cache → Some.
/// None when the provider has no usable models endpoint for this probe.
async fn context_max_output_cached_pair(model: &str, provider: &Provider) -> Option<(u32, u32)> {
    let cached = MODEL_CACHE.lock().ok().and_then(|c| c.get(model).copied());
    if let Some(pair) = cached {
        return Some(pair);
    }
    let fetched = fetch_model_limits(model, provider).await?;
    if let Ok(mut cache) = MODEL_CACHE.lock() {
        cache.insert(model.to_string(), fetched);
    }
    Some(fetched)
}

/// Fetch context window for a model from the provider API.
/// Tries the provider's models endpoint first, falls back to static registry.
pub async fn fetch_context_window(model: &str, provider: &Provider) -> u32 {
    context_max_output_cached_pair(model, provider)
        .await
        .map(|(ctx, _)| ctx)
        .unwrap_or(128_000)
}

/// Fetch max output tokens for a model from the provider API.
pub async fn fetch_max_output(model: &str, provider: &Provider) -> u32 {
    context_max_output_cached_pair(model, provider)
        .await
        .map(|(_, max_out)| max_out)
        .unwrap_or(16_384)
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

fn provider_models_list_client(secs: u64) -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(secs))
        .build()
        .ok()
}

fn model_from_list_by_id<'a>(
    models: &'a [serde_json::Value],
    model_id: &str,
) -> Option<&'a serde_json::Value> {
    models
        .iter()
        .find(|m| m.get("id").and_then(|v| v.as_str()).unwrap_or("") == model_id)
}

/// Bearer GET `/v1/models` (OpenRouter, generic OpenAI-compatible bases).
async fn fetch_bearer_models_list_json(api_key: &str, base_url: &str) -> Option<serde_json::Value> {
    let url = openai_models_url(base_url);
    let client = provider_models_list_client(10)?;
    let resp = client
        .get(&url)
        .header("authorization", format!("Bearer {api_key}"))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json().await.ok()
}

/// Anthropic: GET /v1/models → max_input_tokens, max_tokens
async fn fetch_anthropic_model_limits(
    api_key: &str,
    base_url: &str,
    model: &str,
) -> Option<(u32, u32)> {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let client = provider_models_list_client(10)?;

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
    let models_arr = body.get("data").and_then(|d| d.as_array())?;
    let m = model_from_list_by_id(models_arr, model)?;
    let ctx = m
        .get("max_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(200_000) as u32;
    let max_out = m
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(16_000) as u32;
    Some((ctx, max_out))
}

/// OpenRouter: GET /v1/models → context_length, top_provider.max_completion_tokens
async fn fetch_openrouter_model_limits(
    api_key: &str,
    base_url: &str,
    model: &str,
) -> Option<(u32, u32)> {
    let body = fetch_bearer_models_list_json(api_key, base_url).await?;
    let models_arr = body.get("data").and_then(|d| d.as_array())?;
    let m = model_from_list_by_id(models_arr, model)?;
    let ctx = m
        .get("context_length")
        .and_then(|v| v.as_u64())
        .unwrap_or(128_000) as u32;
    let max_out = m
        .get("top_provider")
        .and_then(|tp| tp.get("max_completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(16_384) as u32;
    Some((ctx, max_out))
}

/// Generic OpenAI-compat: GET /v1/models, using common context fields if exposed.
async fn fetch_openai_compat_model_limits(
    api_key: &str,
    base_url: &str,
    model: &str,
) -> Option<(u32, u32)> {
    let body = fetch_bearer_models_list_json(api_key, base_url).await?;
    let models_arr = body.get("data").and_then(|d| d.as_array())?;
    let m = model_from_list_by_id(models_arr, model)?;
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
    Some((ctx, max_out))
}

/// A model entry returned by a provider's list endpoint.
#[derive(Debug, Clone)]
pub struct RemoteModel {
    pub id: String,
    pub display_name: String,
    pub context_window: u32,
}

/// Fetch available models from a provider's API.
pub async fn fetch_model_list(
    provider_type: &str,
    api_key: &str,
) -> Result<Vec<RemoteModel>, String> {
    match provider_type {
        "anthropic" => fetch_anthropic_model_list(api_key).await,
        "codex" => fetch_codex_model_list(api_key).await,
        "openrouter" => fetch_openrouter_model_list(api_key).await,
        "grok" => fetch_openai_compat_model_list(api_key, oauth::GROK_BASE_URL).await,
        "opencode" => fetch_opencode_model_list(api_key).await,
        "opencode-go" => fetch_opencode_go_model_list(api_key).await,
        "gemini" => gemini::fetch_gemini_model_list(api_key).await,
        _ => Ok(Vec::new()),
    }
}

pub async fn fetch_model_list_for_provider(
    provider: &Provider,
) -> Result<Vec<RemoteModel>, String> {
    match provider {
        Provider::Anthropic {
            api_key, base_url, ..
        } if base_url.contains("opencode.ai/zen/go") => fetch_opencode_go_model_list(api_key).await,
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

async fn fetch_anthropic_model_list(api_key: &str) -> Result<Vec<RemoteModel>, String> {
    let url = "https://api.anthropic.com/v1/models";
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

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
                let detail = format_api_error_body(&text);
                let msg = format!("Anthropic API error ({status}): {detail}");
                crate::broker::try_log_error("models", &format!("API {status}"), Some(&text));
                return Err(msg);
            }
            r
        }
        Err(e) => {
            let msg = format!("Anthropic API request failed: {e}");
            crate::broker::try_log_error("models", "API error", Some(&format!("{e:#}")));
            return Err(msg);
        }
    };

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Anthropic API: invalid JSON response: {e}"))?;

    let mut models = Vec::new();
    if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
        for m in data {
            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let name = m.get("display_name").and_then(|v| v.as_str()).unwrap_or(id);
            let ctx = m
                .get("max_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            if id.is_empty() {
                continue;
            }
            // Offer both 200k and 1M context variants for models that
            // support extended context. Two categories:
            //
            // 1. **Beta-gated** (Sonnet 4, Sonnet 4.5, Opus 4.5): API reports
            //    1M unconditionally but inference >200k needs the
            //    `context-1m-2025-08-07` beta header. Pin base row to 200k.
            //
            // 2. **Native 1M** (Opus 4.7, Opus 4.6, Sonnet 4.6): 1M is the
            //    real default. Still offer a 200k row so users can choose
            //    the lower-context tier (cheaper/faster inference).
            //
            // In both cases the `#1m` variant triggers the beta header at
            // runtime; for native-1M models this is harmless/no-op.
            let gated = supports_1m_context_beta(id);
            let native_1m = !gated && ctx >= 1_000_000;
            if gated || native_1m {
                // Base row: 200k context (no beta header)
                models.push(RemoteModel {
                    id: id.to_string(),
                    display_name: name.to_string(),
                    context_window: 200_000,
                });
                // 1M variant: adds beta header at runtime
                models.push(RemoteModel {
                    id: format!("{id}{ANTHROPIC_1M_SUFFIX}"),
                    display_name: format!("{name} (1M ctx)"),
                    context_window: 1_000_000,
                });
            } else {
                models.push(RemoteModel {
                    id: id.to_string(),
                    display_name: name.to_string(),
                    context_window: ctx,
                });
            }
        }
    }
    Ok(models)
}

/// Suffix appended to a model id to opt the request into the 1M-context beta.
/// Stripped before the id is sent to Anthropic; presence flips on the beta
/// header `context-1m-2025-08-07`.
pub const ANTHROPIC_1M_SUFFIX: &str = "#1m";

/// Models whose 1M-context tier is gated by the beta header. These models
/// report `max_input_tokens=1000000` from the API but inference >200k needs
/// `context-1m-2025-08-07` beta. Newer models (Sonnet 4.6+, Opus 4.6+)
/// advertise 1M natively and are handled separately.
fn supports_1m_context_beta(id: &str) -> bool {
    // Per https://docs.anthropic.com/en/docs/build-with-claude/context-windows
    // Covers: Sonnet 4, Sonnet 4.5, Opus 4.5.
    if id.starts_with("claude-sonnet-4-5-") || id.starts_with("claude-opus-4-5-") {
        return true;
    }
    if let Some(rest) = id.strip_prefix("claude-sonnet-4-") {
        // rest starts with the date (e.g. "20250514") for plain Sonnet 4.
        // Reject minor-version-prefixed IDs ("5-...", "6-...", etc.).
        let next = rest.chars().next().unwrap_or('-');
        return next.is_ascii_digit()
            && rest.len() >= 8
            && !rest.starts_with(|c: char| matches!(c, '5'..='9'));
    }
    false
}

/// Trim and shorten an API error body for inline display. Falls back to the
/// raw text when JSON parsing fails so we never lose the server message.
fn format_api_error_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "(empty body)".to_string();
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(msg) = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|s| s.as_str())
        {
            return msg.to_string();
        }
        if let Some(msg) = v.get("message").and_then(|s| s.as_str()) {
            return msg.to_string();
        }
    }
    if trimmed.len() > 400 {
        format!("{}…", &trimmed[..400])
    } else {
        trimmed.to_string()
    }
}

async fn fetch_codex_model_list(api_key: &str) -> Result<Vec<RemoteModel>, String> {
    let url = "https://chatgpt.com/backend-api/codex/models?client_version=1.0.0";
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

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
                let detail = format_api_error_body(&text);
                let msg = format!("Codex API error ({status}): {detail}");
                crate::broker::try_log_error("models", &format!("API {status}"), Some(&text));
                return Err(msg);
            }
            r
        }
        Err(e) => {
            let msg = format!("Codex API request failed: {e}");
            crate::broker::try_log_error("models", "API error", Some(&format!("{e:#}")));
            return Err(msg);
        }
    };

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Codex API: invalid JSON response: {e}"))?;

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
    Ok(models)
}

async fn fetch_openrouter_model_list(api_key: &str) -> Result<Vec<RemoteModel>, String> {
    fetch_openai_compat_model_list(api_key, "https://openrouter.ai/api/v1").await
}

pub async fn fetch_openai_compat_model_list(
    api_key: &str,
    base_url: &str,
) -> Result<Vec<RemoteModel>, String> {
    if vertex::is_vertex_openapi_base(base_url) {
        return Ok(vertex::fetch_models(api_key, base_url).await);
    }
    let url = openai_models_url(base_url);
    let verbose = is_verbose();
    if verbose {
        eprintln!("\x1b[2m[fetching models from {url}]\x1b[0m");
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let resp = match client
        .get(&url)
        .header("authorization", format!("Bearer {api_key}"))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            if verbose {
                eprintln!("\x1b[33m[model list: {url} returned {status}: {body}]\x1b[0m");
            }
            let detail = format_api_error_body(&body);
            return Err(format!("API error ({status}) at {url}: {detail}"));
        }
        Err(e) => {
            if verbose {
                eprintln!("\x1b[33m[model list: {url} failed: {e}]\x1b[0m");
            }
            return Err(format!("API request to {url} failed: {e}"));
        }
    };

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("API at {url}: invalid JSON response: {e}"))?;

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
    Ok(models)
}

/// Public OpenCode catalog (`/zen/v1` vs `/zen/go/v1`). Zen sends `anthropic-version`; Go does not.
async fn fetch_opencode_public_model_list(
    url: &'static str,
    label: &'static str,
    with_anthropic_version: bool,
) -> Result<Vec<RemoteModel>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let mut req = client.get(url);
    if with_anthropic_version {
        req = req.header("anthropic-version", "2023-06-01");
    }

    let resp = match req.send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            let status = r.status();
            let text = r.text().await.unwrap_or_default();
            let detail = format_api_error_body(&text);
            return Err(format!("{label} API error ({status}): {detail}"));
        }
        Err(e) => return Err(format!("{label} API request failed: {e}")),
    };

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("{label} API: invalid JSON response: {e}"))?;

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
    Ok(models)
}

async fn fetch_opencode_model_list(_api_key: &str) -> Result<Vec<RemoteModel>, String> {
    fetch_opencode_public_model_list("https://opencode.ai/zen/v1/models", "Opencode", true).await
}

async fn fetch_opencode_go_model_list(_api_key: &str) -> Result<Vec<RemoteModel>, String> {
    fetch_opencode_public_model_list("https://opencode.ai/zen/go/v1/models", "OpenCode Go", false)
        .await
}

// ---------------------------------------------------------------------------
// Provider — enum dispatch (no trait, 3 variants)
// ---------------------------------------------------------------------------

// Clone is cheap — every variant's fields are either String
// (owned, clone = alloc+memcpy) or Option<String>. No handles,
// no live connections. Needed so the journaling background task
// can own an Arc<Provider> independent of the REPL's per-turn
// state. If a variant ever gains a non-Clone field (e.g. a live
// websocket), revisit: might need an Arc<Inner> layer instead.
#[derive(Clone)]
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
            base_url: "https://openrouter.ai/api/v1".to_string(),
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

    /// OpenCode Go — budget open-weight models routed through OpenCode Zen
    /// Go's OpenAI-compatible endpoint. Despite exposing an Anthropic-shaped
    /// `/v1/messages` route, that route silently breaks tool calls for most
    /// upstreams (DeepSeek, Kimi, GLM, Mimo); only `/v1/chat/completions`
    /// works end-to-end across the full model list. OpenCode's own client
    /// uses `@ai-sdk/openai-compatible` for the same reason, so we mirror it.
    pub fn opencode_go(api_key: String, credential: Option<String>) -> Self {
        Provider::OpenAiCompat {
            api_key,
            base_url: "https://opencode.ai/zen/go/v1".to_string(),
            provider_type: "opencode-go".to_string(),
            display_name: "opencode-go".to_string(),
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
            Provider::Anthropic { base_url, .. } if base_url.contains("opencode.ai/zen/go") => {
                "opencode-go"
            }
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
            // Gemini: enable cachedContents by default. Users who
            // want to disable (debugging, short-lived sessions) can
            // build a StreamConfig with `gemini_caching: false`
            // explicitly. TTL and min-tokens inherit from
            // StreamConfig::default() (3600s / 4096).
            Provider::Gemini { .. } => StreamConfig {
                gemini_caching: true,
                ..StreamConfig::default()
            },
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
                            eprintln!(
                                "\x1b[2m[credential `{cred}` refreshed after 401, retrying]\x1b[0m"
                            );
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
                api_key,
                base_url,
                credential,
                ..
            } => {
                let key = api_key_override.unwrap_or(api_key);
                let mut cfg = stream_config.clone();
                cfg.credential_kv_key = credential
                    .as_ref()
                    .map(|n| crate::providers::oauth::kv_key_for(n));
                let rx = anthropic::stream(
                    key,
                    base_url,
                    model,
                    system_prompt,
                    messages,
                    tools,
                    prompt_cache_key,
                    &cfg,
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
                    &stream_config,
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
///
/// Used when opening the HTTP stream (`stream_once` / `Provider::stream`) and
/// again for mid-stream recovery in `agent::run` when bytes never reach the UI.
pub(crate) fn is_retryable_error(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    let lower = msg.to_ascii_lowercase();
    // 5xx server errors
    if lower.contains("(500)")
        || lower.contains("(502)")
        || lower.contains("(503)")
        || lower.contains("(504)")
        || lower.contains("(529)")
    {
        return true;
    }
    // 429 rate limit
    if lower.contains("(429)") {
        return true;
    }
    // Connection / transport failures. OS-level reset messages are
    // capitalized ("Connection reset by peer (os error 54)") so we
    // match case-insensitively. `error decoding response body` and
    // `error reading a body from connection` are the hyper/reqwest
    // shapes seen when the stream drops mid-flight.
    if lower.contains("failed to connect")
        || lower.contains("connection reset")
        || lower.contains("connection closed")
        || lower.contains("connection aborted")
        || lower.contains("broken pipe")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("error decoding response body")
        || lower.contains("error reading a body")
        || lower.contains("error reading sse chunk")
        || lower.contains("incomplete message")
        || lower.contains("unexpected eof")
    {
        return true;
    }
    false
}
