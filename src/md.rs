//! Streaming markdown-to-ANSI renderer using pulldown-cmark.
//!
//! Newline-gated: accumulates deltas, only renders complete lines.
//! On finalize, flushes remaining content.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

// ANSI codes
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const ITALIC: &str = "\x1b[3m";
const UNDERLINE: &str = "\x1b[4m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

pub struct MarkdownStream {
    buffer: String,
    committed_line_count: usize,
}

impl MarkdownStream {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            committed_line_count: 0,
        }
    }

    pub fn push(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }

    /// Render complete lines (up to last newline) and return only newly committed lines.
    pub fn commit_complete_lines(&mut self) -> Vec<String> {
        let last_nl = match self.buffer.rfind('\n') {
            Some(i) => i,
            None => return Vec::new(),
        };

        let source = &self.buffer[..=last_nl];
        let rendered = render_markdown(source);

        if self.committed_line_count >= rendered.len() {
            return Vec::new();
        }

        let new_lines = rendered[self.committed_line_count..].to_vec();
        self.committed_line_count = rendered.len();
        new_lines
    }

    /// Flush all remaining content.
    pub fn finalize(&mut self) -> Vec<String> {
        if self.buffer.is_empty() {
            return Vec::new();
        }

        let mut source = self.buffer.clone();
        if !source.ends_with('\n') {
            source.push('\n');
        }

        let rendered = render_markdown(&source);

        let new_lines = if self.committed_line_count >= rendered.len() {
            Vec::new()
        } else {
            rendered[self.committed_line_count..].to_vec()
        };

        self.buffer.clear();
        self.committed_line_count = 0;
        new_lines
    }
}

/// Parse markdown and return ANSI-formatted lines.
fn render_markdown(source: &str) -> Vec<String> {
    let opts = Options::ENABLE_STRIKETHROUGH;
    let parser = Parser::new_ext(source, opts);

    let mut lines: Vec<String> = Vec::new();
    let mut current_line = String::new();
    let mut style_stack: Vec<&str> = Vec::new();
    let mut in_code_block = false;
    let mut code_block_lang = String::new();
    let mut code_block_buf = String::new();
    let mut in_heading = false;
    let mut list_depth: usize = 0;
    let mut ordered_indices: Vec<u64> = Vec::new();

    for event in parser {
        match event {
            Event::Start(tag) => match &tag {
                Tag::Heading { level, .. } => {
                    in_heading = true;
                    if !current_line.is_empty() || !lines.is_empty() {
                        push_line(&mut lines, &mut current_line);
                    }
                    // Heading prefix
                    let marker = "#".repeat(*level as usize);
                    current_line.push_str(&format!("{BOLD}{YELLOW}{marker} "));
                    style_stack.push(RESET);
                }
                Tag::Emphasis => {
                    current_line.push_str(ITALIC);
                    style_stack.push(ITALIC);
                }
                Tag::Strong => {
                    current_line.push_str(BOLD);
                    style_stack.push(BOLD);
                }
                Tag::Strikethrough => {
                    current_line.push_str("\x1b[9m");
                    style_stack.push("\x1b[9m");
                }
                Tag::CodeBlock(kind) => {
                    in_code_block = true;
                    code_block_buf.clear();
                    code_block_lang = match kind {
                        pulldown_cmark::CodeBlockKind::Fenced(lang) => lang.to_string(),
                        _ => String::new(),
                    };
                }
                Tag::Link { dest_url, .. } => {
                    current_line.push_str(&format!("{CYAN}{UNDERLINE}"));
                    style_stack.push(dest_url.to_string().leak());
                }
                Tag::BlockQuote(_) => {
                    style_stack.push(GREEN);
                }
                Tag::List(start) => {
                    list_depth += 1;
                    if let Some(n) = start {
                        ordered_indices.push(*n);
                    } else {
                        ordered_indices.push(0); // 0 = unordered
                    }
                }
                Tag::Item => {
                    let indent = "  ".repeat(list_depth.saturating_sub(1));
                    let marker = match ordered_indices.last().copied() {
                        Some(0) => {
                            format!("{DIM}-{RESET} ")
                        }
                        Some(n) => {
                            if let Some(last) = ordered_indices.last_mut() {
                                *last = n + 1;
                            }
                            format!("{DIM}{n}.{RESET} ")
                        }
                        None => format!("{DIM}-{RESET} "),
                    };
                    current_line.push_str(&format!("{indent}{marker}"));
                }
                Tag::Paragraph => {
                    // Add blank line before paragraph if there's prior content
                    // (but not for the first paragraph, and not inside list items)
                    if !lines.is_empty() && list_depth == 0 {
                        push_line(&mut lines, &mut current_line);
                    }
                    // Apply blockquote styling if inside one
                    for s in &style_stack {
                        if *s == GREEN {
                            current_line.push_str(&format!("{GREEN}> "));
                        }
                    }
                }
                _ => {}
            },
            Event::End(tag_end) => match &tag_end {
                TagEnd::Heading(_) => {
                    current_line.push_str(RESET);
                    push_line(&mut lines, &mut current_line);
                    in_heading = false;
                    style_stack.pop();
                }
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                    current_line.push_str(RESET);
                    style_stack.pop();
                    // Re-apply remaining styles
                    for s in &style_stack {
                        if *s != GREEN {
                            current_line.push_str(s);
                        }
                    }
                }
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    // Render code block
                    if !code_block_lang.is_empty() {
                        push_line(&mut lines, &mut current_line);
                        current_line.push_str(&format!("{DIM}┌─ {code_block_lang}{RESET}"));
                        push_line(&mut lines, &mut current_line);
                    } else {
                        push_line(&mut lines, &mut current_line);
                    }
                    for code_line in code_block_buf.lines() {
                        current_line.push_str(&format!("{DIM}│ {code_line}{RESET}"));
                        push_line(&mut lines, &mut current_line);
                    }
                    if !code_block_lang.is_empty() {
                        current_line.push_str(&format!("{DIM}└─{RESET}"));
                        push_line(&mut lines, &mut current_line);
                    }
                    code_block_buf.clear();
                }
                TagEnd::Link => {
                    current_line.push_str(RESET);
                    style_stack.pop();
                }
                TagEnd::BlockQuote(_) => {
                    style_stack.pop();
                }
                TagEnd::List(_) => {
                    list_depth = list_depth.saturating_sub(1);
                    ordered_indices.pop();
                }
                TagEnd::Item => {}
                TagEnd::Paragraph => {
                    push_line(&mut lines, &mut current_line);
                }
                _ => {}
            },
            Event::Text(text) => {
                if in_code_block {
                    code_block_buf.push_str(&text);
                } else {
                    current_line.push_str(&text);
                }
            }
            Event::Code(code) => {
                current_line.push_str(&format!("{CYAN}`{code}`{RESET}"));
                // Re-apply active styles after reset
                if in_heading {
                    current_line.push_str(&format!("{BOLD}{YELLOW}"));
                }
                for s in &style_stack {
                    if *s != GREEN && *s != RESET {
                        current_line.push_str(s);
                    }
                }
            }
            Event::SoftBreak => {
                current_line.push(' ');
            }
            Event::HardBreak => {
                push_line(&mut lines, &mut current_line);
            }
            Event::Rule => {
                push_line(&mut lines, &mut current_line);
                current_line.push_str(&format!("{DIM}───{RESET}"));
                push_line(&mut lines, &mut current_line);
            }
            _ => {}
        }
    }

    if !current_line.is_empty() {
        push_line(&mut lines, &mut current_line);
    }

    lines
}

fn push_line(lines: &mut Vec<String>, current: &mut String) {
    lines.push(std::mem::take(current));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_heading() {
        let lines = render_markdown("## Hello\n");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("Hello"));
        assert!(lines[0].contains(BOLD));
    }

    #[test]
    fn renders_bold() {
        let lines = render_markdown("**bold text**\n");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains(BOLD));
        assert!(lines[0].contains("bold text"));
    }

    #[test]
    fn renders_inline_code() {
        let lines = render_markdown("use `foo` here\n");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains(CYAN));
        assert!(lines[0].contains("`foo`"));
    }

    #[test]
    fn renders_code_block() {
        let lines = render_markdown("```rust\nlet x = 1;\n```\n");
        assert!(lines.iter().any(|l| l.contains("rust")));
        assert!(lines.iter().any(|l| l.contains("let x = 1;")));
    }

    #[test]
    fn stream_newline_gating() {
        let mut stream = MarkdownStream::new();
        stream.push("Hello **wor");
        assert!(stream.commit_complete_lines().is_empty());

        stream.push("ld**\n");
        let lines = stream.commit_complete_lines();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("world"));
    }

    #[test]
    fn stream_finalize_partial() {
        let mut stream = MarkdownStream::new();
        stream.push("No newline here");
        assert!(stream.commit_complete_lines().is_empty());

        let lines = stream.finalize();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("No newline here"));
    }
}
