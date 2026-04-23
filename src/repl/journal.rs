//! Session journaling — top-level module.
//!
//! Structure (per the implementation plan in
//! `context/todo-journaling.md`):
//!
//!   - `store`   — SQLite CRUD against the v2 journaling tables.
//!                 No LLM, no tokio, no threat scanning. This is
//!                 the only module that touches the DB directly;
//!                 everything else funnels through it.
//!
//! Not yet present (planned, separate commits):
//!   - `prompt`  — build the 12-section summarization prompt.
//!   - `parse`   — parse the LLM's JSON response, defensively.
//!   - `idle`    — arm/cancel the idle timer from the REPL loop.
//!   - `task`    — the background task body: select model,
//!                 run the LLM call, write the row.
//!   - `inject`  — compose the system-prompt suffix on resume.
//!   - `promote` — memory promotion from repeated journal entries.
//!
//! The `runtime::journal()` switch gates everything here. When it
//! returns false, no journaling code runs; the module still
//! compiles and exposes its types so tests and `/journal` can
//! still exercise the CRUD layer.

// Unused until the idle-timer and /journal consumers land in
// subsequent commits. The re-exports exist so higher layers can
// refer to `journal::insert_journal` etc. without reaching through
// the submodule path — once they do, this attribute can come off.
#[allow(dead_code)]
pub(super) mod store;

#[allow(dead_code)]
pub(super) mod prompt;

#[allow(dead_code)]
pub(super) mod parse;

#[allow(dead_code)]
pub(super) mod redact;

#[allow(dead_code)]
pub(super) mod scan;

#[allow(unused_imports)]
pub(super) use store::{
    JournalInsert, JournalRow, insert_journal, latest_to_entry_id, link_memory_to_journal,
    project_tokens_in_window, recent_for_project, recent_for_session, support_count_for_memory,
};
