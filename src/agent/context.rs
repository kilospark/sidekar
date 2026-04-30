//! Per-request context view builder.
//!
//! Builds a right-sized view of history for each API call without mutating
//! canonical history in ways that break the prompt cache prefix.
//!
//! Three optimizations applied in order:
//! 1. Extended-thinking eviction — strip `Thinking` from historic assistant
//!    turns **except** the last assistant and those that emitted tool calls so
//!    OpenAI-compat backends (DeepSeek thinking mode via OpenCode) can replay
//!    `reasoning_content` correctly after tool loops.
//! 2. Tool cycle aging — stub old tool results/arguments beyond the last K
//!    complete tool cycles. The boundary advances only when a new tool cycle
//!    is created (during an agent turn), NOT on every user message, so the
//!    prompt cache prefix stays stable between user turns.
//! 3. Budget trimming — drop oldest messages if still over token budget.
//!
//! Compaction (`compaction::maybe_compact`) fires at ~65% of the context
//! window as a heavier pass that mutates canonical history.

use std::collections::{HashMap, HashSet};

use crate::providers::{ChatMessage, ContentBlock, Role};

/// Number of recent tool cycles to keep intact in `prepare_context`.
/// A "tool cycle" = one assistant message containing ToolCall(s) + the
/// following user message containing matching ToolResult(s).
///
/// Override with `SIDEKAR_KEEP_TOOL_CYCLES` env var.
fn keep_tool_cycles() -> usize {
    static CACHED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("SIDEKAR_KEEP_TOOL_CYCLES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5)
    })
}

/// Minimum byte length for a tool result or tool call arguments to be
/// eligible for aging. Smaller values are kept intact — they're cheap
/// and often useful context (e.g. "File written successfully", `{"command":"ls"}`).
const AGING_MIN_BYTES: usize = 200;

/// Drop `ToolResult` blocks whose `tool_use_id` has no preceding assistant
/// `ToolCall` with the same id. Compaction and budget trimming can leave such
/// orphans behind when they remove an assistant turn but its matching tool
/// result survives in the tail. Anthropic and OpenAI/Codex both reject orphan
/// tool results ("No tool call found for function call output with call_id …").
///
/// Drops user messages that become empty after stripping.
pub(crate) fn drop_orphan_tool_results(messages: &mut Vec<ChatMessage>) {
    let mut seen: HashSet<String> = HashSet::new();
    for msg in messages.iter_mut() {
        match msg.role {
            Role::Assistant => {
                for block in &msg.content {
                    if let ContentBlock::ToolCall { id, .. } = block {
                        seen.insert(id.clone());
                    }
                }
            }
            Role::User => {
                msg.content.retain(|b| match b {
                    ContentBlock::ToolResult { tool_use_id, .. } => seen.contains(tool_use_id),
                    _ => true,
                });
            }
        }
    }
    messages.retain(|m| !m.content.is_empty());
}

/// Rough token estimate: ~4 chars per token (same as compaction).
fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|m| {
            m.content
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } => text.len(),
                    ContentBlock::Thinking { thinking, .. } => thinking.len(),
                    ContentBlock::Reasoning { text } => text.len(),
                    ContentBlock::ToolCall { arguments, .. } => arguments.to_string().len(),
                    ContentBlock::ToolResult { content, .. } => content.len(),
                    ContentBlock::Image { data_base64, .. } => data_base64.len(),
                    ContentBlock::EncryptedReasoning {
                        encrypted_content, ..
                    } => encrypted_content.len() * 3 / 4,
                })
                .sum::<usize>()
        })
        .sum::<usize>()
        / 4
}

/// Age old tool cycles in the ephemeral view by stubbing large ToolResult
/// content and ToolCall arguments. Keeps the last `keep` complete tool
/// cycles intact.
///
/// A "tool cycle" is identified by scanning assistant messages for ToolCall
/// blocks. We count cycles from the tail; once we've seen `keep` cycles,
/// everything older gets aged.
fn age_old_tool_cycles(view: &mut [ChatMessage], keep: usize) {
    // Pass 1: Find the aging boundary.
    //
    // Walk backward through messages counting assistant messages that
    // contain at least one ToolCall. The `keep`-th such message (from
    // the tail) marks the start of the protected window. Everything
    // *before* that index is eligible for aging.
    let mut cycles_seen = 0usize;
    let mut protect_start: Option<usize> = None;

    for (i, msg) in view.iter().enumerate().rev() {
        if msg.role == Role::Assistant
            && msg
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolCall { .. }))
        {
            cycles_seen += 1;
            if cycles_seen >= keep {
                protect_start = Some(i);
                break;
            }
        }
    }

    let boundary = match protect_start {
        Some(0) | None => return, // fewer than `keep` cycles, or nothing before the protected window
        Some(b) => b,             // age view[..b]
    };

    // Pass 2: Build a tool_use_id → tool_name map for aged messages so
    // stubs can include the tool name for context.
    let mut id_to_name: HashMap<String, String> = HashMap::new();
    for msg in &view[..boundary] {
        if msg.role == Role::Assistant {
            for block in &msg.content {
                if let ContentBlock::ToolCall { id, name, .. } = block {
                    id_to_name.insert(id.clone(), name.clone());
                }
            }
        }
    }

    // Pass 3: Stub large blocks in messages before the boundary.
    for msg in view[..boundary].iter_mut() {
        for block in msg.content.iter_mut() {
            match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } if content.len() > AGING_MIN_BYTES => {
                    let tool_name = id_to_name
                        .get(tool_use_id.as_str())
                        .map(|s| s.as_str())
                        .unwrap_or("unknown");
                    *content = format!(
                        "[tool output cleared — {} chars, tool: {tool_name}]",
                        content.len()
                    );
                }
                ContentBlock::ToolCall { arguments, .. }
                    if arguments.to_string().len() > AGING_MIN_BYTES =>
                {
                    *arguments = serde_json::Value::Object(serde_json::Map::new());
                }
                _ => {}
            }
        }
    }
}

/// Build an ephemeral view of history with thinking eviction, tool cycle
/// aging, and optional budget trimming. Canonical history is not mutated.
pub fn prepare_context(history: &[ChatMessage], token_budget: usize) -> Vec<ChatMessage> {
    // --- Step 0: Drop orphan tool results (compaction/trimming can leave them) ---
    let mut view: Vec<ChatMessage> = history.to_vec();
    drop_orphan_tool_results(&mut view);

    // --- Step 1: Thinking block eviction (ephemeral, view-only) ---
    //
    // Anthropic-scale extended-thinking blocks inflate prompts; we strip them
    // from historic assistant turns — but assistants that emitted **tool calls**
    // may need that text echoed back (`reasoning_content`) on gateways that wrap
    // DeepSeek-thinking models (OpenCode Zen/Go).

    let last_assistant_idx = view
        .iter()
        .rposition(|m| m.role == Role::Assistant)
        .unwrap_or(usize::MAX);

    for (i, msg) in view.iter_mut().enumerate() {
        if msg.role != Role::Assistant || i == last_assistant_idx {
            continue;
        }
        let tool_assistant_turn = msg
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolCall { .. }));
        if tool_assistant_turn {
            continue;
        }
        msg.content
            .retain(|b| !matches!(b, ContentBlock::Thinking { .. }));
    }
    // Drop messages that became empty after stripping.
    view.retain(|m| !m.content.is_empty());

    // --- Step 2: Tool cycle aging (ephemeral, view-only) ---
    age_old_tool_cycles(&mut view, keep_tool_cycles());

    // --- Step 3: Budget trimming (ephemeral, view-only) ---
    let est = estimate_tokens(&view);
    if est > token_budget && view.len() > 2 {
        // Protect the first message (may contain session context) and the last 5.
        let protect_tail = 5.min(view.len());
        let drop_to = view.len().saturating_sub(protect_tail);
        let mut drop_from = 1; // skip first message
        let mut saved = 0usize;

        while drop_from < drop_to && est.saturating_sub(saved) > token_budget {
            saved += estimate_tokens(std::slice::from_ref(&view[drop_from]));
            drop_from += 1;
        }

        if drop_from > 1 {
            let dropped = drop_from - 1;
            let mut trimmed = Vec::with_capacity(view.len() - dropped + 1);
            trimmed.push(view[0].clone());
            trimmed.push(ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: format!("[{dropped} earlier messages removed to fit context budget]"),
                }],
            });
            trimmed.extend(view[drop_from..].iter().cloned());
            view = trimmed;
            drop_orphan_tool_results(&mut view);
        }
    }

    view
}

#[cfg(test)]
mod tests;
