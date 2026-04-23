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

mod commands;
mod hygiene;
mod store;
mod util;

pub use commands::cmd_memory;
pub use commands::startup_brief;
use hygiene::*;
use store::*;
use util::*;

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

#[cfg(test)]
mod tests;
