//! Session journaling — top-level module.
//!
//! The `store` submodule is SQLite CRUD for the v2 journaling tables (no LLM,
//! no tokio, no threat scanning). Future work (`prompt`, `parse`, `idle`,
//! `task`, `inject`, `promote`) is outlined in `context/todo-journaling.md`.
//!
//! The `runtime::journal()` flag gates execution. When it is off, no journaling
//! runs; the module still exposes types so tests and `/journal` can hit CRUD.

// Unused until the idle-timer and /journal consumers land in
// subsequent commits. The re-exports exist so higher layers can
// refer to `journal::insert_journal` etc. without reaching through
// the submodule path — once they do, this attribute can come off.
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

pub(super) mod promote;

pub(crate) use idle::IdleTracker;

#[allow(unused_imports)]
pub(super) use store::{
    JournalInsert, JournalRow, insert_journal, latest_to_entry_id, link_memory_to_journal,
    project_tokens_in_window, recent_for_project, recent_for_session, support_count_for_memory,
};
