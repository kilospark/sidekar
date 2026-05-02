use crate::*;
use rusqlite::{OptionalExtension, params};

const MEMORY_TYPES: &[&str] = &[
    "decision",
    "convention",
    "constraint",
    "preference",
    "open-thread",
    "artifact-pointer",
];

const TAG_RULES: &[(&str, &[&str])] = &[
    (
        "testing",
        &[
            "test",
            "spec",
            "jest",
            "vitest",
            "mocha",
            "cypress",
            "playwright",
            "assert",
            "coverage",
        ],
    ),
    (
        "typescript",
        &["typescript", "tsconfig", "interface ", "generic<", "enum "],
    ),
    (
        "database",
        &[
            "sql",
            "postgres",
            "mysql",
            "sqlite",
            "mongodb",
            "redis",
            "migration",
            "schema",
            "orm",
            "drizzle",
            "prisma",
        ],
    ),
    (
        "api",
        &[
            "endpoint",
            "rest",
            "graphql",
            "grpc",
            "fetch(",
            "axios",
            "api route",
            "middleware",
        ],
    ),
    (
        "auth",
        &[
            "auth",
            "login",
            "session",
            "token",
            "jwt",
            "oauth",
            "password",
            "credential",
        ],
    ),
    (
        "deployment",
        &[
            "deploy",
            "docker",
            "kubernetes",
            "k8s",
            "github actions",
            "vercel",
            "aws",
            "gcp",
            "fly.io",
        ],
    ),
    (
        "workflow",
        &[
            "workflow",
            "process",
            "automation",
            "script",
            "hook",
            "lint",
            "format",
        ],
    ),
    (
        "security",
        &[
            "security",
            "xss",
            "injection",
            "sanitize",
            "encrypt",
            "secret",
            "credential",
        ],
    ),
    (
        "browser",
        &[
            "chrome",
            "browser",
            "tab",
            "dom",
            "selector",
            "cookie",
            "service worker",
            "cdp",
        ],
    ),
];

#[derive(Debug, Clone)]
struct MemoryEventRow {
    id: i64,
    project: String,
    event_type: String,
    scope: String,
    summary: String,
    confidence: f64,
    reinforcement_count: i64,
    tags: Vec<String>,
    superseded_by: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Clone)]
struct SearchResultRow {
    row: MemoryEventRow,
    score: f64,
}

mod candidates;
mod commands;
mod hygiene;
mod import;
mod store;
mod util;

pub(crate) use candidates::process_journal_candidates;
pub use commands::cmd_memory;
pub use commands::startup_brief;
use hygiene::*;
use store::*;
use util::*;

pub struct RelevantMemoryBrief {
    pub text: String,
    pub ids: Vec<i64>,
}

/// Public entry for programmatic callers that need to write a
/// memory event. Wraps `store::write_memory_event` so external
/// modules (e.g. the journal promoter) don't have to reach
/// across submodule visibility.
///
/// Argument contract matches `store::write_memory_event` verbatim:
///   - `project`: scope project name, or GLOBAL_SCOPE constant.
///   - `event_type`: one of MEMORY_TYPES (decision/convention/
///     constraint/preference/open-thread/artifact-pointer).
///   - `scope`: "project" or "global".
///   - `summary`: free-form text, user-visible.
///   - `confidence`: 0.0..=1.0. Direct-authored entries default
///     to ~0.75 elsewhere in the codebase; promotions use lower.
///   - `user_tags`: additional tag strings; auto-tags are merged
///     in by the store layer.
///   - `trigger_kind` / `source_kind`: provenance hints. Active
///     writes from user commands pass "active"/"user";
///     background promotions pass "passive"/"journal".
///
/// Returns the same human-readable message the store produces
/// ("Stored memory [N]." / "Deduplicated existing memory [N].").
#[allow(clippy::too_many_arguments)]
pub fn write_memory_event(
    project: &str,
    event_type: &str,
    scope: &str,
    summary: &str,
    confidence: f64,
    user_tags: &[String],
    trigger_kind: &str,
    source_kind: &str,
) -> anyhow::Result<String> {
    store::write_memory_event(
        project,
        event_type,
        scope,
        summary,
        confidence,
        user_tags,
        trigger_kind,
        source_kind,
    )
}

pub fn relevant_brief(
    project: &str,
    hint: &str,
    limit: usize,
) -> anyhow::Result<RelevantMemoryBrief> {
    let query = hint.trim();
    if query.is_empty() {
        return Ok(RelevantMemoryBrief {
            text: String::new(),
            ids: Vec::new(),
        });
    }

    let mut matches = search_events(
        query,
        crate::scope::ScopeView::Project,
        Some(project),
        None,
        limit.saturating_mul(3).max(limit),
    )?;
    for term in path_like_terms(query) {
        matches.extend(search_events(
            &term,
            crate::scope::ScopeView::Project,
            Some(project),
            None,
            limit,
        )?);
    }
    matches.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let rows = dedupe_rows_by_norm(matches.into_iter().map(|item| item.row).collect());
    if rows.is_empty() {
        return Ok(RelevantMemoryBrief {
            text: String::new(),
            ids: Vec::new(),
        });
    }

    let mut lines = vec!["## Relevant Memory".to_string()];
    let mut ids = Vec::new();
    for row in rows.into_iter().take(limit) {
        ids.push(row.id);
        let scope = if row.scope == crate::scope::GLOBAL_SCOPE {
            " [global]"
        } else {
            ""
        };
        lines.push(format!("- [{}] {}{}", row.event_type, row.summary, scope));
    }

    Ok(RelevantMemoryBrief {
        text: lines.join("\n"),
        ids,
    })
}

pub fn log_selected_memories(
    ids: &[i64],
    session_id: &str,
    entry_id: Option<&str>,
    hint: &str,
) -> anyhow::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let detail = serde_json::json!({ "hint": hint }).to_string();
    for id in ids {
        log_memory_usage(
            *id,
            Some(session_id),
            None,
            entry_id,
            "selected",
            Some(&detail),
        )?;
    }
    Ok(())
}

pub fn accept_selected_memories(
    ids: &[i64],
    session_id: &str,
    entry_id: Option<&str>,
    hint: &str,
) -> anyhow::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let detail = serde_json::json!({ "hint": hint }).to_string();
    for id in ids {
        log_memory_usage(
            *id,
            Some(session_id),
            None,
            entry_id,
            "selected",
            Some(&detail),
        )?;
        log_memory_usage(
            *id,
            Some(session_id),
            None,
            entry_id,
            "accepted",
            Some(&detail),
        )?;
    }
    reinforce_events(ids.iter().copied())?;
    Ok(())
}

fn path_like_terms(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in query.split_whitespace() {
        let cleaned = token
            .trim_matches(|ch: char| {
                matches!(
                    ch,
                    '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':'
                )
            })
            .trim();
        if cleaned.contains('/') || cleaned.contains('.') {
            out.push(cleaned.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests;
