pub mod anthropic;
pub mod codex;
pub mod oauth;

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

    eprintln!("\x1b[2m--- API Request ---");
    eprintln!("POST {url}");
    eprintln!("Headers: {headers:?}");
    eprintln!(
        "Body: {}",
        serde_json::to_string_pretty(body).unwrap_or_default()
    );
    eprintln!("---\x1b[0m");
}

pub(super) fn log_api_error(status: StatusCode, text: &str) {
    if !is_verbose() {
        return;
    }

    eprintln!("\x1b[2m--- API Error {status} ---\n{text}\n---\x1b[0m");
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    Thinking {
        thinking: String,
        signature: String,
    },
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

// ---------------------------------------------------------------------------
// Model metadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: &'static str,
    pub display_name: &'static str,
    pub provider: ProviderKind,
    pub context_window: u32,
    pub max_output: u32,
    pub supports_thinking: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProviderKind {
    Anthropic,
    Codex,
}

pub static MODELS: &[ModelInfo] = &[
    // Anthropic (Claude subscription)
    ModelInfo {
        id: "claude-opus-4-20250514",
        display_name: "Claude Opus 4",
        provider: ProviderKind::Anthropic,
        context_window: 200_000,
        max_output: 32_000,
        supports_thinking: true,
    },
    ModelInfo {
        id: "claude-sonnet-4-20250514",
        display_name: "Claude Sonnet 4",
        provider: ProviderKind::Anthropic,
        context_window: 200_000,
        max_output: 16_000,
        supports_thinking: true,
    },
    ModelInfo {
        id: "claude-sonnet-4-6-20250514",
        display_name: "Claude Sonnet 4.6",
        provider: ProviderKind::Anthropic,
        context_window: 200_000,
        max_output: 16_000,
        supports_thinking: true,
    },
    ModelInfo {
        id: "claude-haiku-4-5-20251001",
        display_name: "Claude Haiku 4.5",
        provider: ProviderKind::Anthropic,
        context_window: 200_000,
        max_output: 8_192,
        supports_thinking: false,
    },
    // OpenAI Codex (ChatGPT subscription)
    ModelInfo {
        id: "gpt-5.1-codex-mini",
        display_name: "GPT-5.1 Codex Mini",
        provider: ProviderKind::Codex,
        context_window: 272_000,
        max_output: 128_000,
        supports_thinking: true,
    },
    ModelInfo {
        id: "gpt-5.2-codex",
        display_name: "GPT-5.2 Codex",
        provider: ProviderKind::Codex,
        context_window: 272_000,
        max_output: 128_000,
        supports_thinking: true,
    },
    ModelInfo {
        id: "gpt-5.3-codex",
        display_name: "GPT-5.3 Codex",
        provider: ProviderKind::Codex,
        context_window: 272_000,
        max_output: 128_000,
        supports_thinking: true,
    },
    ModelInfo {
        id: "gpt-5.4-mini",
        display_name: "GPT-5.4 Mini",
        provider: ProviderKind::Codex,
        context_window: 272_000,
        max_output: 128_000,
        supports_thinking: true,
    },
];

pub fn model_info(id: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.id == id)
}

pub fn default_model() -> &'static str {
    "claude-sonnet-4-20250514"
}

// ---------------------------------------------------------------------------
// Provider — enum dispatch (no trait, 2 variants)
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

    pub async fn stream(
        &self,
        model: &str,
        system_prompt: &str,
        messages: &[ChatMessage],
        tools: &[ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::UnboundedReceiver<StreamEvent>> {
        match self {
            Provider::Anthropic { api_key, base_url } => {
                anthropic::stream(api_key, base_url, model, system_prompt, messages, tools).await
            }
            Provider::Codex { api_key, account_id, base_url } => {
                codex::stream(api_key, account_id, base_url, model, system_prompt, messages, tools).await
            }
        }
    }
}
