use super::*;

// ---------------------------------------------------------------------------
// Stream event rendering
// ---------------------------------------------------------------------------

/// Stateful renderer with streaming markdown support, tool call display, and spinner.
pub(super) struct EventRenderer {
    md: crate::md::MarkdownStream,
    tool_args: std::collections::HashMap<usize, (String, String)>,
    spinner: Option<Spinner>,
    partial_visible: bool,
}

impl EventRenderer {
    pub(super) fn new(_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self {
            md: crate::md::MarkdownStream::new(),
            tool_args: std::collections::HashMap::new(),
            spinner: None,
            partial_visible: false,
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
        for line in lines {
            self.emitln(&line);
        }
        let _ = io::stdout().flush();
    }

    fn update_partial_preview(&mut self) {
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
    }

    pub(super) fn render(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::Waiting => {
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
                self.md.push(delta);
                self.flush_md_lines();
                self.update_partial_preview();
            }
            StreamEvent::ThinkingDelta { .. } => {
                // Stream has started — replace any earlier status label
                // ("connecting to model", "resolving context", "working")
                // with "thinking" so the spinner reflects reality.
                self.set_status_spinner("thinking");
            }
            StreamEvent::ToolCallStart { index, name, .. } => {
                self.stop_spinner();
                self.clear_partial_preview();
                for line in self.md.finalize() {
                    self.emitln(&line);
                }
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
                // Restart spinner while tool executes and next API call happens
                self.stop_spinner();
                self.spinner = Some(Spinner::start_with_label("working".to_string()));
            }
            StreamEvent::Done { message } => {
                self.stop_spinner();
                self.clear_partial_preview();
                for line in self.md.finalize() {
                    self.emitln(&line);
                }
                self.emitln("");
                let _ = io::stdout().flush();
                let u = &message.usage;
                if u.cache_read_tokens > 0 || u.cache_write_tokens > 0 {
                    self.emitln(&format!(
                        "\x1b[2m[{} in / {} out / {} cache read / {} cache write tokens]\x1b[0m",
                        u.input_tokens, u.output_tokens, u.cache_read_tokens, u.cache_write_tokens
                    ));
                } else {
                    self.emitln(&format!(
                        "\x1b[2m[{} in / {} out tokens]\x1b[0m",
                        u.input_tokens, u.output_tokens
                    ));
                }
                let _ = io::stdout().flush();
            }
            StreamEvent::Error { message } => {
                self.stop_spinner();
                self.clear_partial_preview();
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

pub(super) struct Spinner {
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

const SPINNER_FRAMES: &[&str] = &[
    "[    ]", "[=   ]", "[==  ]", "[=== ]", "[ ===]", "[  ==]", "[   =]", "[    ]",
];
const SPINNER_COLOR: &str = "\x1b[36m";

impl Spinner {
    pub(super) fn start_with_label(label: String) -> Self {
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let r = running.clone();
        let handle = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let label_part = if label.is_empty() {
                String::new()
            } else {
                format!(" {label}")
            };
            let mut i = 0;
            while r.load(std::sync::atomic::Ordering::Relaxed) {
                let elapsed = started.elapsed().as_secs_f32();
                let line = format!(
                    "{SPINNER_COLOR}{} {:.1}s{label_part}\x1b[0m",
                    SPINNER_FRAMES[i % SPINNER_FRAMES.len()],
                    elapsed,
                );
                emit_transient_status(&line);
                i += 1;
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
            clear_transient_status();
        });
        Self {
            running,
            handle: Some(handle),
        }
    }

    pub(super) fn stop(&mut self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
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
            picked.map(str::to_string).unwrap_or_else(|| args_json.to_string())
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
