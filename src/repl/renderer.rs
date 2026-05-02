use super::spinner::Spinner;
use super::*;
use crate::providers::ContentBlock;

// ---------------------------------------------------------------------------
// Stream event rendering
// ---------------------------------------------------------------------------

/// Emit one line of model reasoning — dim + italic so it reads softer than answer text.
fn emit_dim_thinking_line(line: &str) {
    let sanitized: String = line.chars().map(|c| if c == '\r' { ' ' } else { c }).collect();
    let styled = format!("\x1b[2m\x1b[3m{sanitized}\x1b[0m\r\n");
    emit_shared_output(&styled);
}

fn emit_thinking_blocks_from_message(content: &[ContentBlock]) {
    for block in content {
        match block {
            ContentBlock::Thinking { thinking, .. } if !thinking.trim().is_empty() => {
                for line in thinking.lines() {
                    emit_dim_thinking_line(line);
                }
            }
            ContentBlock::Reasoning { text } if !text.trim().is_empty() => {
                for line in text.lines() {
                    emit_dim_thinking_line(line);
                }
            }
            _ => {}
        }
    }
}

/// Stateful renderer with streaming markdown support, tool call display, and spinner.
pub(super) struct EventRenderer {
    md: crate::md::MarkdownStream,
    tool_args: std::collections::HashMap<usize, (String, String)>,
    spinner: Option<Spinner>,
    partial_visible: bool,
    /// Last time `update_partial_preview` actually re-rendered. Each token
    /// triggers `render_markdown` over the full pending region — O(N²) in
    /// paragraph length. Capping refresh rate keeps long outputs responsive
    /// without losing the trailing-line preview.
    last_preview_at: Option<std::time::Instant>,
    /// Pending extended-thinking characters until the next `\n`.
    thinking_line_buf: String,
    /// True once any `ThinkingDelta` arrived this provider round — skips duplicating
    /// thinking blocks from `Done.message.content`.
    thinking_streamed: bool,
}

/// Minimum interval between partial-preview rerenders. ~30 Hz — fast enough
/// that the trailing line still feels live, slow enough that an LLM streaming
/// 50+ tokens/sec doesn't trigger a full markdown reparse on every delta.
const PREVIEW_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(33);

/// Emit committed markdown lines in one editor-mutex acquisition (ANSI clear
/// and redraw once). Avoids calling `emit_shared_line` per line, which would
/// repaint the prompt after each line during large flushes.
fn emit_lines_batched(lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let mut joined = String::with_capacity(lines.iter().map(|l| l.len() + 2).sum());
    for line in lines {
        joined.push_str(line);
        // \r\n: raw mode (cfmakeraw clears OPOST) doesn't translate \n to
        // CR+LF, so each line needs an explicit \r to land at column 0.
        joined.push_str("\r\n");
    }
    emit_shared_output(&joined);
}

impl EventRenderer {
    pub(super) fn new(_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self {
            md: crate::md::MarkdownStream::new(),
            tool_args: std::collections::HashMap::new(),
            spinner: None,
            partial_visible: false,
            last_preview_at: None,
            thinking_line_buf: String::new(),
            thinking_streamed: false,
        }
    }

    /// Write text + newline to stdout and relay tunnel.
    fn emitln(&self, text: &str) {
        emit_shared_line(text);
    }

    fn stop_spinner(&mut self) {
        if let Some(mut s) = self.spinner.take() {
            s.stop();
        }
    }

    fn set_status_spinner(&mut self, label: &str) {
        self.stop_spinner();
        self.spinner = Some(Spinner::start_with_label(label.to_string()));
    }

    pub(super) fn teardown(&mut self) {
        self.stop_spinner();
        self.clear_partial_preview();
    }

    fn flush_md_lines(&mut self) {
        let lines = self.md.commit_complete_lines();
        if lines.is_empty() {
            return;
        }
        self.clear_partial_preview();
        emit_lines_batched(&lines);
        let _ = io::stdout().flush();
    }

    /// Refresh the trailing-line preview. Throttled to `PREVIEW_MIN_INTERVAL`
    /// because each call re-parses the entire uncommitted region — at LLM
    /// streaming speeds (~50 tokens/sec) without throttling this dominates
    /// the per-delta budget.
    ///
    /// `force = true` bypasses the throttle (used at stream end / cancel).
    fn update_partial_preview(&mut self, force: bool) {
        if !force {
            let now = std::time::Instant::now();
            if let Some(last) = self.last_preview_at
                && now.duration_since(last) < PREVIEW_MIN_INTERVAL
            {
                return;
            }
            self.last_preview_at = Some(now);
        } else {
            self.last_preview_at = Some(std::time::Instant::now());
        }
        match self.md.preview_partial_line() {
            Some(line) => {
                emit_transient_status(&line);
                let _ = io::stdout().flush();
                self.partial_visible = true;
            }
            None => self.clear_partial_preview(),
        }
    }

    fn clear_partial_preview(&mut self) {
        if self.partial_visible {
            clear_transient_status();
            let _ = io::stdout().flush();
            self.partial_visible = false;
        }
        self.last_preview_at = None;
    }

    fn reset_thinking_for_new_provider_round(&mut self) {
        self.thinking_line_buf.clear();
        self.thinking_streamed = false;
    }

    fn flush_thinking_complete_lines(&mut self) {
        while let Some(pos) = self.thinking_line_buf.find('\n') {
            let line = self.thinking_line_buf[..pos].to_string();
            self.thinking_line_buf.drain(..=pos);
            emit_dim_thinking_line(&line);
        }
    }

    /// Any in-progress thinking text before assistant markdown or tools.
    fn flush_thinking_remainder_as_line(&mut self) {
        if self.thinking_line_buf.is_empty() {
            return;
        }
        let line = std::mem::take(&mut self.thinking_line_buf);
        emit_dim_thinking_line(&line);
    }

    fn flush_thinking_before_assistant_body(&mut self) {
        self.flush_thinking_complete_lines();
        self.flush_thinking_remainder_as_line();
    }

    pub(super) fn render(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::Waiting => {
                self.reset_thinking_for_new_provider_round();
                self.set_status_spinner("waiting for response");
            }
            StreamEvent::ResolvingContext => {
                self.set_status_spinner("resolving context");
            }
            StreamEvent::Connecting => {
                self.set_status_spinner("connecting");
            }
            StreamEvent::Compacting => {
                self.set_status_spinner("compacting context");
            }
            StreamEvent::Idle => {
                self.stop_spinner();
            }
            StreamEvent::ToolExec {
                name,
                arguments_json,
            } => {
                self.stop_spinner();
                let detail = extract_tool_summary(name, arguments_json);
                let label = if detail.is_empty() {
                    format!("running {name}")
                } else {
                    format!("running {name} — {detail}")
                };
                self.spinner = Some(Spinner::start_with_label(label));
            }
            StreamEvent::TextDelta { delta } => {
                self.stop_spinner();
                self.clear_partial_preview();
                self.flush_thinking_before_assistant_body();
                self.md.push(delta);
                self.flush_md_lines();
                self.update_partial_preview(false);
            }
            StreamEvent::ThinkingDelta { delta } => {
                self.clear_partial_preview();
                self.stop_spinner();
                self.thinking_streamed = true;
                self.thinking_line_buf.push_str(delta);
                self.flush_thinking_complete_lines();
            }
            StreamEvent::ToolCallStart { index, name, .. } => {
                self.stop_spinner();
                self.clear_partial_preview();
                self.flush_thinking_before_assistant_body();
                let lines = self.md.finalize();
                emit_lines_batched(&lines);
                let _ = io::stdout().flush();
                match self.tool_args.entry(*index) {
                    Entry::Vacant(v) => {
                        v.insert((name.clone(), String::new()));
                    }
                    Entry::Occupied(mut o) => {
                        o.get_mut().0 = name.clone();
                    }
                }
                self.spinner = Some(Spinner::start_with_label(format!("preparing {name}")));
            }
            StreamEvent::ToolCallDelta { index, delta } => {
                let (_, args) = self
                    .tool_args
                    .entry(*index)
                    .or_insert_with(|| (String::new(), String::new()));
                args.push_str(delta);
                if self.spinner.is_none() {
                    self.spinner = Some(Spinner::start_with_label("preparing tool".to_string()));
                }
            }
            StreamEvent::ToolCallEnd { index } => {
                if let Some((name, args_json)) = self.tool_args.remove(index) {
                    if crate::runtime::verbose() {
                        let display_name = if name.is_empty() {
                            "tool"
                        } else {
                            name.as_str()
                        };
                        let detail = extract_tool_summary(display_name, &args_json);
                        self.emitln(&format!(
                            "\n\x1b[2m└─\x1b[0m \x1b[36m{display_name}\x1b[0m \x1b[2m{detail}\x1b[0m"
                        ));
                        let _ = io::stdout().flush();
                    }
                }
                // Restart spinner while tool executes and next API call happens
                self.stop_spinner();
                self.spinner = Some(Spinner::start_with_label("working".to_string()));
            }
            StreamEvent::Done { message } => {
                self.stop_spinner();
                self.clear_partial_preview();
                self.flush_thinking_complete_lines();
                self.flush_thinking_remainder_as_line();
                if !self.thinking_streamed {
                    emit_thinking_blocks_from_message(&message.content);
                }
                let lines = self.md.finalize();
                emit_lines_batched(&lines);
                let _ = io::stdout().flush();
                // Always-on dim footer (not gated on `--verbose`): tokens +
                // optional cache breakdown + provider rate-limit snapshot when present.
                let u = &message.usage;
                let rl = crate::repl::ratelimit::format_compact(message.rate_limit.as_ref());
                if u.cache_read_tokens > 0 || u.cache_write_tokens > 0 {
                    self.emitln(&format!(
                        "\x1b[2m[{} in / {} out / {} cache read / {} cache write tokens{}]\x1b[0m",
                        u.input_tokens,
                        u.output_tokens,
                        u.cache_read_tokens,
                        u.cache_write_tokens,
                        rl
                    ));
                } else {
                    self.emitln(&format!(
                        "\x1b[2m[{} in / {} out tokens{}]\x1b[0m",
                        u.input_tokens, u.output_tokens, rl
                    ));
                }
                let _ = io::stdout().flush();
            }
            StreamEvent::Error { message } => {
                self.stop_spinner();
                self.clear_partial_preview();
                self.flush_thinking_complete_lines();
                self.flush_thinking_remainder_as_line();
                self.emitln(&format!("\n\x1b[31mError: {message}\x1b[0m"));
                let _ = io::stdout().flush();
            }
        }
    }
}

impl Drop for EventRenderer {
    fn drop(&mut self) {
        self.teardown();
    }
}

pub(super) fn extract_tool_summary(name: &str, args_json: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(args_json).unwrap_or_default();
    let raw = match name {
        "bash" | "Bash" => args
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or(args_json)
            .to_string(),
        "Sidekar" | "sidekar" => args
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| args_json.to_string()),
        _ => {
            // Prefer common identifier-like fields over arbitrary string values
            // (e.g. Edit's `new_string` would otherwise win alphabetically and
            // spill multi-line code into the spinner line).
            const PREFERRED: &[&str] = &[
                "path",
                "file_path",
                "pattern",
                "url",
                "query",
                "key",
                "name",
                "id",
            ];
            let obj = args.as_object();
            let picked = obj.and_then(|o| {
                PREFERRED
                    .iter()
                    .find_map(|k| o.get(*k).and_then(|v| v.as_str()))
                    .or_else(|| o.values().find_map(|v| v.as_str()))
            });
            picked
                .map(str::to_string)
                .unwrap_or_else(|| args_json.to_string())
        }
    };
    truncate_display(&raw, 120)
}

/// Truncate to `max` chars and collapse to a single line — the transient
/// spinner status can only safely occupy one row.
fn truncate_display(s: &str, max: usize) -> String {
    let first = s.lines().next().unwrap_or(s);
    let cleaned: String = first
        .chars()
        .map(|c| if c == '\t' { ' ' } else { c })
        .collect();
    if cleaned.len() <= max {
        cleaned
    } else {
        format!("{}...", &cleaned[..cleaned.floor_char_boundary(max)])
    }
}
