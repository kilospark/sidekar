//! Streaming markdown-to-ANSI renderer using pulldown-cmark.
//!
//! Commits at *block boundaries* — blank lines outside any fenced code
//! block, or the line following a closing fence. Markdown is block-oriented:
//! once a block is closed, no future delta can change how its content renders.
//! Committing earlier (e.g. on every newline) breaks for emphasis that spans
//! lines, unclosed code fences, setext headings, and tables.

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
    /// Byte offset into `buffer` up to which content has been rendered and
    /// returned to the caller as committed lines. Everything before this
    /// offset is immutable from the renderer's perspective.
    committed_byte_offset: usize,
    /// Whether any committed output has been emitted yet. Used to decide
    /// whether to prepend a blank separator line between consecutive blocks.
    first_commit_done: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    Emphasis,
    Strong,
    Strikethrough,
    Link,
    BlockQuote,
}

impl Default for MarkdownStream {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkdownStream {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            committed_byte_offset: 0,
            first_commit_done: false,
        }
    }

    pub fn push(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }

    /// Render any newly-complete blocks and return their lines. A block is
    /// considered complete when a blank line follows it (outside any fenced
    /// code block) or a code fence closes. Between block boundaries this
    /// returns an empty vec — the caller should use `preview_partial_line`
    /// for transient display of the in-progress block.
    pub fn commit_complete_lines(&mut self) -> Vec<String> {
        let commit_end = find_safe_commit_end(&self.buffer, self.committed_byte_offset);
        if commit_end <= self.committed_byte_offset {
            return Vec::new();
        }

        let segment = &self.buffer[self.committed_byte_offset..commit_end];
        let mut rendered = render_markdown(segment);
        self.committed_byte_offset = commit_end;

        if rendered.is_empty() {
            return Vec::new();
        }

        if self.first_commit_done {
            // Each segment is rendered in isolation, so leading blank lines
            // that render.rs would normally insert between paragraphs/tables
            // aren't emitted. Restore that spacing ourselves.
            rendered.insert(0, String::new());
        } else {
            self.first_commit_done = true;
        }

        rendered
    }

    /// Flush all remaining content, including any incomplete trailing block.
    /// Resets internal state so the stream can be reused for a new message.
    pub fn finalize(&mut self) -> Vec<String> {
        let pending_start = self.committed_byte_offset;
        let had_prior_commit = self.first_commit_done;

        let mut rendered = if pending_start < self.buffer.len() {
            let segment = &self.buffer[pending_start..];
            render_markdown(segment)
        } else {
            Vec::new()
        };

        self.buffer.clear();
        self.committed_byte_offset = 0;
        self.first_commit_done = false;

        if rendered.is_empty() {
            return Vec::new();
        }

        if had_prior_commit {
            rendered.insert(0, String::new());
        }
        rendered
    }

    /// Render the in-progress trailing block and return its last non-empty
    /// line for transient display. Always terminates with a RESET so any
    /// unclosed ANSI style (mid-emphasis, mid-code) doesn't bleed into the
    /// terminal's subsequent output.
    pub fn preview_partial_line(&self) -> Option<String> {
        if self.committed_byte_offset >= self.buffer.len() {
            return None;
        }
        let pending = &self.buffer[self.committed_byte_offset..];
        if pending.trim().is_empty() {
            return None;
        }

        let rendered = render_markdown(pending);
        let line = rendered.into_iter().rev().find(|l| !l.is_empty())?;
        Some(format!("{line}{RESET}"))
    }
}

/// Returns the byte offset (absolute in `source`) up to which rendered
/// output is "safe to commit" — i.e. no future tokens appended to the
/// buffer can change how the content before this offset renders.
///
/// Safe boundaries:
///   * A blank line outside any fenced code block (ends a block in
///     CommonMark — the preceding content cannot merge into a later block).
///   * A closing fence line for a fenced code block.
///
/// Scans from `from` forward; returns `from` if no safe boundary is reached.
fn find_safe_commit_end(source: &str, from: usize) -> usize {
    let mut offset = from;
    let mut last_safe = from;
    let mut in_fence = false;
    let mut fence_marker: char = '`';
    let mut fence_len: usize = 0;

    for line in source[from..].split_inclusive('\n') {
        if !line.ends_with('\n') {
            // Trailing partial line — not a safe boundary.
            break;
        }
        let content = &line[..line.len() - 1];
        let trimmed_start = content.trim_start();

        if in_fence {
            let count = trimmed_start.chars().take_while(|&c| c == fence_marker).count();
            if count >= fence_len && trimmed_start[count..].trim().is_empty() {
                in_fence = false;
                fence_len = 0;
                offset += line.len();
                last_safe = offset;
                continue;
            }
            offset += line.len();
            continue;
        }

        // Outside a fence — detect opening fence.
        let backticks = trimmed_start.chars().take_while(|&c| c == '`').count();
        let tildes = trimmed_start.chars().take_while(|&c| c == '~').count();
        if backticks >= 3 {
            in_fence = true;
            fence_marker = '`';
            fence_len = backticks;
            offset += line.len();
            continue;
        }
        if tildes >= 3 {
            in_fence = true;
            fence_marker = '~';
            fence_len = tildes;
            offset += line.len();
            continue;
        }

        if content.trim().is_empty() {
            offset += line.len();
            last_safe = offset;
            continue;
        }

        offset += line.len();
    }

    last_safe
}

#[cfg(test)]
mod tests;
