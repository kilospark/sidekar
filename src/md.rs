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

mod render;

use render::*;

pub struct MarkdownStream {
    buffer: String,
    committed_line_count: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    Emphasis,
    Strong,
    Strikethrough,
    Link,
    BlockQuote,
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
    /// Withholds trailing pipe-lines until a separator confirms them as a table.
    pub fn commit_complete_lines(&mut self) -> Vec<String> {
        let last_nl = match self.buffer.rfind('\n') {
            Some(i) => i,
            None => return Vec::new(),
        };

        let source = &self.buffer[..=last_nl];
        let safe = safe_commit_end(source);
        if safe == 0 {
            return Vec::new();
        }

        let rendered = render_markdown(&source[..safe]);

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

    /// Render the currently buffered trailing partial line, if any.
    pub fn preview_partial_line(&self) -> Option<String> {
        if self.buffer.is_empty() || self.buffer.ends_with('\n') {
            return None;
        }

        let rendered = render_markdown(&self.buffer);
        if self.committed_line_count >= rendered.len() {
            return None;
        }

        let pending = &rendered[self.committed_line_count..];
        if pending.len() == 1 {
            Some(pending[0].clone())
        } else {
            None
        }
    }
}

/// Find the safe byte offset to commit up to, withholding trailing pipe-lines
/// that could be an unconfirmed table (header without separator yet).
fn safe_commit_end(source: &str) -> usize {
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() {
        return source.len();
    }

    // Scan backward to find trailing block of pipe-lines
    let mut pipe_start = lines.len();
    while pipe_start > 0 && lines[pipe_start - 1].trim_start().starts_with('|') {
        pipe_start -= 1;
    }

    if pipe_start == lines.len() {
        // No trailing pipe lines
        return source.len();
    }

    // Check if the pipe block contains a separator (confirmed table)
    let has_separator = lines[pipe_start..].iter().any(|l| {
        let t = l.trim();
        t.starts_with('|') && t.len() > 2 && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
    });

    if has_separator {
        return source.len();
    }

    // Withhold the unconfirmed pipe block — find byte offset of pipe_start line
    if pipe_start == 0 {
        return 0;
    }
    let mut offset = 0;
    for (i, line) in source.lines().enumerate() {
        if i == pipe_start {
            break;
        }
        offset += line.len() + 1; // +1 for the newline
    }
    offset
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
    fn renders_link_without_url_leak() {
        let lines = render_markdown("[docs](https://example.com/docs)\n");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("docs"));
        assert!(!lines[0].contains("https://example.com/docs"));
    }

    #[test]
    fn nested_link_styles_reapply_without_url_text() {
        let lines = render_markdown("**[docs](https://example.com/docs)** and `code`\n");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("docs"));
        assert!(lines[0].contains("`code`"));
        assert!(!lines[0].contains("https://example.com/docs"));
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

    #[test]
    fn previews_partial_line_before_newline() {
        let mut stream = MarkdownStream::new();
        stream.push("Hello **world");
        let preview = stream.preview_partial_line().expect("partial preview");
        assert!(preview.contains("Hello"));
        assert!(preview.contains("world"));
    }

    #[test]
    fn renders_table() {
        let md = "| Symbol | Price | Change |\n|--------|-------|--------|\n| AAPL | 150 | +2% |\n| GOOG | 2800 | -1% |\n";
        let lines = render_markdown(md);
        assert_eq!(lines.len(), 4);
        assert!(lines[0].contains("Symbol"));
        assert!(lines[0].contains("Price"));
        assert!(lines[0].contains("Change"));
        assert!(lines[1].contains("---"));
        assert!(lines[2].contains("AAPL"));
        assert!(lines[2].contains("150"));
        assert!(lines[3].contains("GOOG"));
        assert!(lines[3].contains("2800"));
    }

    #[test]
    fn streaming_table_withheld_until_separator() {
        let mut stream = MarkdownStream::new();

        // Header line arrives — should be withheld (no separator yet)
        stream.push("**Plays:**\n\n| Ticker | What |\n");
        let lines = stream.commit_complete_lines();
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|l| l.contains("Plays:")));
        // The pipe line must NOT be committed yet
        assert!(!lines.iter().any(|l| l.contains("Ticker")));

        // Separator arrives — now table is confirmed, header + separator emitted
        stream.push("|--------|------|\n");
        let lines = stream.commit_complete_lines();
        assert!(lines.iter().any(|l| l.contains("Ticker")));
        assert!(lines.iter().any(|l| l.contains("---")));

        // Data row
        stream.push("| AGQ | 2x Silver |\n");
        let lines = stream.commit_complete_lines();
        assert!(lines.iter().any(|l| l.contains("AGQ")));

        // Finalize
        let lines = stream.finalize();
        assert!(lines.is_empty() || lines.iter().all(|l| l.is_empty()));
    }
}
