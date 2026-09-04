//! Session journaling — top-level module.
//!
//! The `store` submodule is SQLite CRUD for the journaling tables (no LLM,
//! no tokio, no threat scanning). The pipeline that sits on top of it is
//! `prefilter` -> `prompt` -> `redact` -> `parse` -> `scan` -> `store`,
//! driven by `idle` and `task`, with `inject` reading journals back into a
//! resumed session. Design and rationale: `context/journaling.md`.
//!
//! Promotion of repeated journal entries into `memory_events` is *not* here.
//! It runs through `memory::process_journal_candidates` (`src/memory/
//! candidates.rs`), called from `task::run_once` after the journal row is
//! inserted. That path supersedes the earlier `journal::promote` module,
//! which covered only constraints and decisions and had no reinforcement,
//! contradiction, or review surface.
//!
//! The `runtime::journal()` flag gates execution. When it is off, no journaling
//! runs; the module still exposes types so tests and `/journal` can hit CRUD.

// `store` and `parse` are reached from src/commands/journal.rs
// (the `sidekar journal` CLI), so they need pub(crate). The other
// submodules are internal orchestration and stay pub(super).
pub(crate) mod store;

pub(super) mod prompt;

pub(crate) mod parse;

pub(super) mod redact;

pub(super) mod scan;

pub(super) mod prefilter;

pub(super) mod idle;

pub(super) mod task;

pub(super) mod inject;

pub(crate) use inject::build_injection_block;

pub(crate) use idle::IdleTracker;
