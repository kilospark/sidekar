//! Structured event parser for agent PTY output.
//!
//! Accumulates raw PTY bytes into lines, strips ANSI for classification,
//! and emits semantic `AgentEvent`s that clients can render natively
//! instead of running a full terminal emulator.

use serde::Serialize;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum AgentEvent {
    /// Assistant text (markdown-capable).
    #[serde(rename = "text")]
    Text { content: String },

    /// Tool invocation header (e.g. "Read src/main.rs", "Bash ls -la").
    #[serde(rename = "tool_call")]
    ToolCall { tool: String, input: String },

    /// Tool output / result block.
    #[serde(rename = "tool_result")]
    ToolResult { content: String },

    /// Fenced code block.
    #[serde(rename = "code")]
    Code { language: String, content: String },

    /// Diff hunk (unified diff lines).
    #[serde(rename = "diff")]
    Diff { content: String },

    /// Status/progress indicator (thinking, running, etc.).
    #[serde(rename = "status")]
    Status { state: String },
}

// ---------------------------------------------------------------------------
// Line classification
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum LineKind {
    Empty,
    ToolHeader,
    ToolOutput,
    DiffAdd,
    DiffRemove,
    DiffMeta,
    CodeFence,
    Status,
    Text,
}

/// Strip all ANSI escape sequences from a byte slice, returning plain text.
fn strip_ansi(raw: &[u8]) -> String {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == 0x1b {
            i += 1;
            if i < raw.len() {
                match raw[i] {
                    b'[' => {
                        // CSI sequence: ESC [ ... final_byte
                        i += 1;
                        while i < raw.len() && !(0x40..=0x7e).contains(&raw[i]) {
                            i += 1;
                        }
                        if i < raw.len() {
                            i += 1; // skip final byte
                        }
                    }
                    b']' => {
                        // OSC sequence: ESC ] ... ST (BEL or ESC \)
                        i += 1;
                        while i < raw.len() {
                            if raw[i] == 0x07 {
                                i += 1;
                                break;
                            }
                            if raw[i] == 0x1b && i + 1 < raw.len() && raw[i + 1] == b'\\' {
                                i += 2;
                                break;
                            }
                            i += 1;
                        }
                    }
                    b'(' | b')' => {
                        // Charset designation: ESC ( X or ESC ) X
                        i += 1;
                        if i < raw.len() {
                            i += 1;
                        }
                    }
                    _ => {
                        i += 1; // skip single char after ESC
                    }
                }
            }
        } else if raw[i] == b'\r' {
            // Skip carriage return
            i += 1;
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Classify a single plain-text line (ANSI already stripped).
fn classify_line(plain: &str) -> LineKind {
    let trimmed = plain.trim();

    if trimmed.is_empty() {
        return LineKind::Empty;
    }

    // Status indicators — lines that get overwritten (progress spinners, etc.)
    // These typically start with common status chars or contain spinner patterns
    if trimmed.starts_with("⠋")
        || trimmed.starts_with("⠙")
        || trimmed.starts_with("⠹")
        || trimmed.starts_with("⠸")
        || trimmed.starts_with("⠼")
        || trimmed.starts_with("⠴")
        || trimmed.starts_with("⠦")
        || trimmed.starts_with("⠧")
        || trimmed.starts_with("⠇")
        || trimmed.starts_with("⠏")
    {
        return LineKind::Status;
    }

    // Tool headers — lines with box-drawing or common tool markers
    if trimmed.starts_with("─")
        || trimmed.starts_with("━")
        || trimmed.starts_with("╭")
        || trimmed.starts_with("╰")
        || trimmed.starts_with("┌")
        || trimmed.starts_with("└")
        || trimmed.starts_with("│")
        || trimmed.starts_with("┃")
    {
        // Check if this looks like a tool header vs. just box drawing
        if contains_tool_keyword(trimmed) {
            return LineKind::ToolHeader;
        }
        return LineKind::ToolOutput;
    }

    // Tool call patterns: "⏎ ToolName arguments" or "● ToolName"
    if trimmed.starts_with("⏎") || trimmed.starts_with("●") || trimmed.starts_with("◆") {
        return LineKind::ToolHeader;
    }

    // Diff lines
    if trimmed.starts_with("+++") || trimmed.starts_with("---") || trimmed.starts_with("@@") {
        return LineKind::DiffMeta;
    }
    if trimmed.starts_with('+') && !trimmed.starts_with("++") {
        return LineKind::DiffAdd;
    }
    if trimmed.starts_with('-') && !trimmed.starts_with("--") {
        return LineKind::DiffRemove;
    }

    // Code fences
    if trimmed.starts_with("```") {
        return LineKind::CodeFence;
    }

    // Tool output: indented lines or lines starting with common output markers
    if plain.starts_with("  ") || plain.starts_with('\t') {
        return LineKind::ToolOutput;
    }

    // Shell prompts / command output
    if trimmed.starts_with("$ ") || trimmed.starts_with("> ") {
        return LineKind::ToolOutput;
    }

    LineKind::Text
}

fn contains_tool_keyword(s: &str) -> bool {
    let lower = s.to_lowercase();
    lower.contains("read")
        || lower.contains("write")
        || lower.contains("edit")
        || lower.contains("bash")
        || lower.contains("glob")
        || lower.contains("grep")
        || lower.contains("search")
        || lower.contains("tool")
}

// ---------------------------------------------------------------------------
// Event parser (stateful line accumulator)
// ---------------------------------------------------------------------------

pub struct EventParser {
    /// Partial line buffer (bytes not yet terminated by newline).
    partial: Vec<u8>,
    /// Current block accumulator.
    block: Vec<String>,
    /// Classification of the current block.
    block_kind: Option<LineKind>,
    /// Inside a fenced code block.
    in_code_fence: bool,
    /// Language of the current code fence.
    code_language: String,
    /// Code block content accumulator.
    code_content: Vec<String>,
}

impl Default for EventParser {
    fn default() -> Self {
        Self::new()
    }
}

impl EventParser {
    pub fn new() -> Self {
        Self {
            partial: Vec::new(),
            block: Vec::new(),
            block_kind: None,
            in_code_fence: false,
            code_language: String::new(),
            code_content: Vec::new(),
        }
    }

    /// Feed raw PTY bytes. Returns any events that are now complete.
    pub fn feed(&mut self, data: &[u8]) -> Vec<AgentEvent> {
        let mut events = Vec::new();

        for &byte in data {
            if byte == b'\n' {
                let line_bytes = std::mem::take(&mut self.partial);
                let plain = strip_ansi(&line_bytes);
                self.process_line(&plain, &mut events);
            } else {
                self.partial.push(byte);
            }
        }

        events
    }

    /// Flush any pending partial line / block. Call when the session ends
    /// or when you want to force-emit pending content.
    pub fn flush(&mut self) -> Vec<AgentEvent> {
        let mut events = Vec::new();

        // Flush partial line
        if !self.partial.is_empty() {
            let line_bytes = std::mem::take(&mut self.partial);
            let plain = strip_ansi(&line_bytes);
            if !plain.trim().is_empty() {
                self.process_line(&plain, &mut events);
            }
        }

        // Flush pending code block
        if self.in_code_fence && !self.code_content.is_empty() {
            events.push(AgentEvent::Code {
                language: std::mem::take(&mut self.code_language),
                content: self.code_content.join("\n"),
            });
            self.code_content.clear();
            self.in_code_fence = false;
        }

        // Flush pending block
        self.flush_block(&mut events);

        events
    }

    fn process_line(&mut self, plain: &str, events: &mut Vec<AgentEvent>) {
        // Handle code fences
        if plain.trim().starts_with("```") {
            if self.in_code_fence {
                // Closing fence — emit code block
                events.push(AgentEvent::Code {
                    language: std::mem::take(&mut self.code_language),
                    content: self.code_content.join("\n"),
                });
                self.code_content.clear();
                self.in_code_fence = false;
                return;
            } else {
                // Opening fence — flush current block, start code block
                self.flush_block(events);
                self.in_code_fence = true;
                let after_fence = plain.trim().trim_start_matches('`');
                self.code_language = after_fence.trim().to_string();
                return;
            }
        }

        if self.in_code_fence {
            self.code_content.push(plain.to_string());
            return;
        }

        let kind = classify_line(plain);

        // Status lines are emitted immediately (they're ephemeral)
        if kind == LineKind::Status {
            self.flush_block(events);
            events.push(AgentEvent::Status {
                state: plain.trim().to_string(),
            });
            return;
        }

        // Empty lines: flush current block
        if kind == LineKind::Empty {
            self.flush_block(events);
            return;
        }

        // If kind changes from current block, flush and start new
        if let Some(current_kind) = self.block_kind
            && !kinds_compatible(current_kind, kind)
        {
            self.flush_block(events);
        }

        self.block_kind = Some(kind);
        self.block.push(plain.to_string());
    }

    fn flush_block(&mut self, events: &mut Vec<AgentEvent>) {
        if self.block.is_empty() {
            self.block_kind = None;
            return;
        }

        let content = std::mem::take(&mut self.block).join("\n");
        let kind = self.block_kind.take().unwrap_or(LineKind::Text);

        match kind {
            LineKind::ToolHeader => {
                let (tool, input) = parse_tool_header(&content);
                events.push(AgentEvent::ToolCall { tool, input });
            }
            LineKind::ToolOutput => {
                events.push(AgentEvent::ToolResult { content });
            }
            LineKind::DiffAdd | LineKind::DiffRemove | LineKind::DiffMeta => {
                events.push(AgentEvent::Diff { content });
            }
            LineKind::Text => {
                events.push(AgentEvent::Text { content });
            }
            _ => {
                events.push(AgentEvent::Text { content });
            }
        }
    }
}

/// Whether two line kinds can coexist in the same block.
fn kinds_compatible(a: LineKind, b: LineKind) -> bool {
    match (a, b) {
        // Diff lines can mix
        (
            LineKind::DiffAdd | LineKind::DiffRemove | LineKind::DiffMeta,
            LineKind::DiffAdd | LineKind::DiffRemove | LineKind::DiffMeta,
        ) => true,
        // Tool output lines can accumulate
        (LineKind::ToolOutput, LineKind::ToolOutput) => true,
        // Text lines can accumulate
        (LineKind::Text, LineKind::Text) => true,
        // Otherwise, different kind = new block
        (a, b) => a == b,
    }
}

/// Extract tool name and input from a tool header line.
fn parse_tool_header(content: &str) -> (String, String) {
    let trimmed = content.trim();

    // Try patterns like "⏎ Read src/main.rs" or "● Bash(ls -la)"
    for prefix in &["⏎ ", "● ", "◆ "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let parts: Vec<&str> = rest
                .splitn(2, |c: char| c.is_whitespace() || c == '(')
                .collect();
            let tool = parts[0].to_string();
            let input = if parts.len() > 1 {
                parts[1].trim_end_matches(')').trim().to_string()
            } else {
                String::new()
            };
            return (tool, input);
        }
    }

    // Try box-drawing header: "── Read file.txt ──"
    let stripped = trimmed
        .trim_start_matches(|c: char| "─━┌└╭╰│┃ ".contains(c))
        .trim_end_matches(|c: char| "─━┐┘╮╯│┃ ".contains(c))
        .trim();
    if !stripped.is_empty() {
        let parts: Vec<&str> = stripped.splitn(2, char::is_whitespace).collect();
        return (
            parts[0].to_string(),
            parts.get(1).unwrap_or(&"").to_string(),
        );
    }

    ("unknown".to_string(), trimmed.to_string())
}

// ---------------------------------------------------------------------------
// Wire format: wraps events for tunnel transmission
// ---------------------------------------------------------------------------

/// Serialize a content event for tunnel transmission as a JSON text frame.
/// Shape: `{"ch":"events","v":1,"event":{kind:"text"|"tool_call"|...}}`.
pub fn event_to_json(event: &AgentEvent) -> String {
    serde_json::json!({
        "ch": "events",
        "v": 1,
        "event": event,
    })
    .to_string()
}

/// Serialize a turn-lifecycle marker. The `event` field is a bare string so
/// relay consumers (e.g. Telegram viewer's `is_turn_boundary`) can match on
/// `*_complete` / `*_done` / `turn_end` / `assistant_message` suffixes
/// without having to descend into a tagged enum.
///
/// Recognized names (keep in sync with relay/src/telegram.rs::is_turn_boundary):
///   - "turn_start"          — agent began processing a user input
///   - "tool_call_start"     — a tool is about to execute
///   - "tool_call_end"       — a tool has finished executing
///   - "assistant_complete"  — assistant's turn has ended (flush trigger)
///   - "turn_end"            — synonym for assistant_complete
///   - "error"               — assistant stream errored
pub fn lifecycle_to_json(name: &str) -> String {
    serde_json::json!({
        "ch": "events",
        "v": 1,
        "event": name,
    })
    .to_string()
}

#[cfg(test)]
mod tests;
