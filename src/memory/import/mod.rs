//! `sidekar memory import` — scan local agent configs and session
//! history, extract durable memories (decisions, conventions,
//! preferences, constraints), and write them through the existing
//! `memory` store so dedup + supersession apply automatically.
//!
//! Design notes:
//!
//! * Idempotent. Every extracted memory flows through
//!   `memory::write_memory_event` whose hash + FTS + word-overlap
//!   checks merge re-imports into reinforcement. Re-running the
//!   command after a week simply bumps confidence on anything
//!   still present and adds new memories.
//!
//! * Per-source isolation. One broken parser (e.g. Cursor schema
//!   drift) cannot abort the whole run. Every detector returns a
//!   `SourceReport` with errors collected; the summary at the end
//!   enumerates them.
//!
//! * No LLM in v1 for structured sources. Manifest extractors
//!   (package.json / Cargo.toml / pyproject.toml / README.md /
//!   ADR / CONTRIBUTING) are deterministic Rust. Freeform text
//!   (CLAUDE.md, .cursorrules, session transcripts) goes through
//!   an optional LLM pass gated on `--no-llm`.
//!
//! * The import log (`memory_import_log`) stores per-file SHA-256
//!   hashes so a second invocation short-circuits unchanged files.
//!   A fresh `batch_id` groups every run for later audit/undo.

mod commands;
mod extract_llm;
mod extract_structured;
mod parse_transcripts;
mod sources;

use std::collections::BTreeMap;
use std::path::PathBuf;

pub use self::commands::cmd_memory_import;

/// What kind of memory did we extract and where did it come from?
/// Fed straight into `write_memory_event`'s `source_kind` column
/// so the provenance of every imported event is queryable later.
#[derive(Debug, Clone)]
pub(super) struct Candidate {
    pub event_type: String,
    pub summary: String,
    pub scope: String,
    pub project: String,
    pub confidence: f64,
    pub tags: Vec<String>,
    pub source_kind: String,
    pub source_file: PathBuf,
}

/// Per-source run outcome. Errors are captured rather than raised
/// so the summary can report partial success.
#[derive(Debug, Default)]
pub(super) struct SourceReport {
    pub label: String,
    pub files_seen: usize,
    pub files_skipped_unchanged: usize,
    pub candidates: Vec<Candidate>,
    pub errors: Vec<String>,
}

impl SourceReport {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            ..Default::default()
        }
    }
}

/// Parsed CLI flags. Populated from `cmd_memory_import` before the
/// scan phase; immutable afterwards.
#[derive(Debug, Clone)]
pub(super) struct ImportOptions {
    pub sources: Vec<String>,
    pub project_override: Option<String>,
    pub scope_filter: ScopeFilter,
    pub since_secs: Option<u64>,
    pub max_sessions: usize,
    pub no_llm: bool,
    pub credential: Option<String>,
    pub model: Option<String>,
    pub dry_run: bool,
    pub assume_yes: bool,
    pub verbose: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScopeFilter {
    Project,
    Global,
    All,
}

impl ImportOptions {
    /// Apply the scope filter to an in-flight candidate. Sources
    /// emit candidates with their natural scope; the filter just
    /// drops whatever the user said to ignore.
    pub fn keep_scope(&self, scope: &str) -> bool {
        match self.scope_filter {
            ScopeFilter::All => true,
            ScopeFilter::Project => scope == crate::scope::PROJECT_SCOPE,
            ScopeFilter::Global => scope == crate::scope::GLOBAL_SCOPE,
        }
    }
}

/// Aggregated stats for the final summary line.
#[derive(Debug, Default)]
pub(super) struct WriteStats {
    pub written: usize,
    pub deduped: usize,
    pub errors: Vec<String>,
}

/// Pretty per-source breakdown for `--dry-run` and the final summary.
pub(super) fn format_report_table(reports: &[SourceReport]) -> String {
    if reports.is_empty() {
        return "No sources detected.".to_string();
    }
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    let mut lines = Vec::new();
    lines.push(format!(
        "{:<14} {:>6} {:>8} {:>10}",
        "source", "files", "skipped", "candidates"
    ));
    for r in reports {
        lines.push(format!(
            "{:<14} {:>6} {:>8} {:>10}",
            r.label,
            r.files_seen,
            r.files_skipped_unchanged,
            r.candidates.len()
        ));
        for c in &r.candidates {
            *by_type.entry(c.event_type.clone()).or_insert(0) += 1;
        }
    }
    if !by_type.is_empty() {
        lines.push(String::new());
        lines.push("Candidate breakdown:".to_string());
        for (ty, count) in &by_type {
            lines.push(format!("  {ty}: {count}"));
        }
    }
    lines.join("\n")
}
