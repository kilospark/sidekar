//! Standard output pipeline for sidekar commands.
//!
//! Commands produce a value implementing [`CommandOutput`]. The dispatcher
//! renders it as plain text (default, human-readable), JSON (machines), or
//! TOON (token-efficient LLM input) based on [`runtime::output_format`].
//! The format is selected globally via `--format=<name>`, `--json`, or
//! `--toon`, and commands opt in by calling [`emit`] or [`to_string`].

use std::io::{self, Write};

use serde::Serialize;

/// Wire format selected by `--format=` / `--json` / `--toon` / `--markdown`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    Toon,
    Markdown,
}

impl OutputFormat {
    /// Parse a format name (case-insensitive). Returns `None` for unknown.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "text" | "txt" | "plain" => Some(Self::Text),
            "json" => Some(Self::Json),
            "toon" => Some(Self::Toon),
            "markdown" | "md" => Some(Self::Markdown),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
            Self::Toon => "toon",
            Self::Markdown => "markdown",
        }
    }
}

/// A command result that can be rendered in every supported format.
///
/// Implementers derive `Serialize` for JSON/TOON and hand-write `render_text`
/// for the human path.
pub trait CommandOutput: Serialize {
    fn render_text(&self, w: &mut dyn Write) -> io::Result<()>;

    /// Render markdown output. Defaults to `render_text` so commands without a
    /// distinct markdown representation fall back gracefully. Commands that
    /// produce richer markdown (fenced code blocks, headings) should override.
    fn render_markdown(&self, w: &mut dyn Write) -> io::Result<()> {
        self.render_text(w)
    }
}

/// Render `value` into `w` using the currently-selected output format.
///
/// Text rendering uses the command's `render_text` implementation; JSON uses
/// `serde_json::to_string_pretty`; TOON uses `toon_format::encode_default`.
/// Structured formats always emit a trailing newline so the output streams
/// cleanly when piped.
pub fn render<T: CommandOutput>(value: &T, w: &mut dyn Write) -> anyhow::Result<()> {
    match crate::runtime::output_format() {
        OutputFormat::Text => {
            value.render_text(w)?;
        }
        OutputFormat::Json => {
            let s = serde_json::to_string_pretty(value)?;
            writeln!(w, "{s}")?;
        }
        OutputFormat::Toon => {
            let s = toon_format::encode::encode_default(value)
                .map_err(|e| anyhow::anyhow!("toon encode failed: {e}"))?;
            writeln!(w, "{s}")?;
        }
        OutputFormat::Markdown => {
            value.render_markdown(w)?;
        }
    }
    Ok(())
}

/// Render directly to stdout. Convenient for main/binary-level commands.
pub fn emit<T: CommandOutput>(value: &T) -> anyhow::Result<()> {
    let mut out = io::stdout().lock();
    render(value, &mut out)
}

/// Render into a `String` so the output can flow through the buffered
/// `AppContext::output` pipeline (via `out!`) without changing command
/// signatures.
pub fn to_string<T: CommandOutput>(value: &T) -> anyhow::Result<String> {
    let mut buf: Vec<u8> = Vec::new();
    render(value, &mut buf)?;
    Ok(String::from_utf8(buf)?.trim_end_matches('\n').to_string())
}

/// A generic text-only envelope. Commands whose output is inherently
/// free-form prose (DOM dumps, extracted page text, status lines) wrap their
/// payload in this type so the pipeline still produces valid JSON / TOON.
#[derive(Serialize)]
pub struct PlainOutput {
    pub text: String,
}

impl PlainOutput {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

impl CommandOutput for PlainOutput {
    fn render_text(&self, w: &mut dyn Write) -> io::Result<()> {
        writeln!(w, "{}", self.text)
    }
}

#[cfg(test)]
mod tests;
