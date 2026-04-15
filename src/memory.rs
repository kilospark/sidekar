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

#[cfg(test)]
mod tests;
