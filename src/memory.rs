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
mod store;
mod util;

pub use commands::cmd_memory;
pub use commands::startup_brief;
use store::*;
use util::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn with_test_home<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = crate::test_home_lock()
            .lock()
            .map_err(|_| anyhow!("failed to lock test HOME mutex"))?;

        let old_home = env::var_os("HOME");
        let temp_home = env::temp_dir().join(format!("sidekar-memory-test-{}", now_epoch_ms()));
        fs::create_dir_all(&temp_home)?;

        // Safety: tests run under a process-global mutex and restore HOME before returning.
        unsafe { env::set_var("HOME", &temp_home) };

        let result = f();

        match old_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
        let _ = fs::remove_dir_all(&temp_home);
        result
    }

    #[test]
    fn search_normalizes_punctuation_for_fts() -> Result<()> {
        with_test_home(|| {
            write_memory_event(
                "alpha",
                "convention",
                "project",
                "Use Readability.js before scraping article text",
                0.8,
                &[],
                "explicit",
                "user",
            )?;

            let results = search_events(
                "Readability.js",
                crate::scope::ScopeView::Project,
                Some("alpha"),
                None,
                5,
            )?;
            assert_eq!(results.len(), 1);
            assert_eq!(
                results[0].row.summary,
                "Use Readability.js before scraping article text"
            );
            Ok(())
        })
    }

    #[test]
    fn detect_patterns_promotes_global_memory() -> Result<()> {
        with_test_home(|| {
            write_memory_event(
                "alpha",
                "convention",
                "project",
                "Use Readability.js before scraping article text",
                0.8,
                &[],
                "explicit",
                "user",
            )?;
            write_memory_event(
                "beta",
                "convention",
                "project",
                "Use Readability.js before scraping article text",
                0.8,
                &[],
                "explicit",
                "user",
            )?;

            assert_eq!(detect_patterns(2)?, 1);

            let conn = crate::broker::open_db()?;
            let global_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM memory_events
                 WHERE scope = 'global' AND event_type = 'convention'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(global_count, 1);

            let global_summary: String = conn.query_row(
                "SELECT summary FROM memory_events
                 WHERE scope = 'global' AND event_type = 'convention'
                 LIMIT 1",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(
                global_summary,
                "Use Readability.js before scraping article text"
            );
            Ok(())
        })
    }
}
