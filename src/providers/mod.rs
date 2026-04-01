pub mod anthropic;
pub mod codex;
pub mod oauth;

use serde::{Deserialize, Serialize};

static VERBOSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_verbose(v: bool) {
    VERBOSE.store(v, std::sync::atomic::Ordering::SeqCst);
}

pub fn is_verbose() -> bool {
    VERBOSE.load(std::sync::atomic::Ordering::SeqCst)
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
