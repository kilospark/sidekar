//! CRUD against `session_journals` and `memory_journal_support`.
//!
//! Pure storage layer — no LLM, no tokio, no threat scanning. Higher
//! layers (idle trigger, background task, memory promoter) build on
//! these primitives. Mirroring the pattern from `src/session.rs`
//! rather than inventing a new one: `Result<T>` everywhere, rusqlite
//! params, `broker::open()` for the connection.
//!
//! What lives here:
//!   - `JournalEntry` / `JournalEntryRow` — insert+return shape.
//!   - `insert_journal` — write one row, return autoincrement id.
//!   - `recent_for_session` — last N journals for a session, newest
//!     first. Used by session-resume injection and `/journal`.
//!   - `recent_for_project` — last N across all sessions in a cwd.
//!     Used by the optional "last seen in this project" startup
//!     brief and by the cross-session memory promoter.
//!   - `latest_to_entry_id` — what was the upper bound of the last
//!     journaling pass for this session, so the next pass can
//!     resume from strictly-after that point.
//!   - `project_tokens_in_window` — sum of tokens_in over a time
//!     window, for the per-project cost cap enforcement.
//!   - `link_memory_to_journal` — insert into `memory_journal_
//!     support`; used by the promoter to record the evidence chain.
//!
//! What *doesn't* live here: update/delete. Journals are append-only
//! by design; a bad row is rare enough that raw `sqlite3 broker.db
//! DELETE …` is an acceptable escape hatch. Keeping the surface
//! minimal reduces the chance that a later refactor breaks recall.

// Suppressed until the consumer modules (idle trigger, background
// task, promoter, inject) land in follow-up commits — those are the
// callers for `support_count_for_memory`, `row_to_journal`, etc.
// Re-enable (or delete this attr) once all consumers exist.
#![allow(dead_code)]

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::broker;

/// Fields for a write. Caller constructs, we persist, return the
/// assigned autoincrement id. Mirrors the shape of the row columns
/// except for id (we assign) and created_at (we stamp with "now"
/// unless the caller provides it — convenient for tests).
#[derive(Debug, Clone)]
pub struct JournalInsert<'a> {
    pub session_id: &'a str,
    pub project: &'a str,
    /// Inclusive lower bound of the repl_entries range this journal
    /// covers. UUID string, matches `repl_entries.id`.
    pub from_entry_id: &'a str,
    /// Inclusive upper bound. Next journal pass resumes strictly
    /// after this id in `created_at` order (see
    /// `latest_to_entry_id`).
    pub to_entry_id: &'a str,
    /// Serialized 12-section summary. This module does not validate
    /// its shape — higher layers produce and parse it. We store
    /// whatever string the caller passes.
    pub structured_json: &'a str,
    /// One-liner for fast rendering (e.g. /session teaser). Caller
    /// extracts this from structured_json once, we store once.
    pub headline: &'a str,
    /// Previous journal id for this session, for iterative-update
    /// chains. None for the first journal of a session.
    pub previous_id: Option<i64>,
    pub model_used: &'a str,
    pub cred_used: &'a str,
    pub tokens_in: i64,
    pub tokens_out: i64,
    /// Optional explicit timestamp (unix secs f64). Provided by tests;
    /// production callers pass `None` and we use `SystemTime::now()`.
    pub created_at: Option<f64>,
}

/// Row read back from a SELECT. Includes the server-assigned id and
/// `created_at` so callers can render "Xh ago" etc. without a
/// second query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalRow {
    pub id: i64,
    pub session_id: String,
    pub project: String,
    pub created_at: f64,
    pub from_entry_id: String,
    pub to_entry_id: String,
    pub structured_json: String,
    pub headline: String,
    pub previous_id: Option<i64>,
    pub model_used: String,
    pub cred_used: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
}

fn now_secs() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Insert one journal row, return its assigned id.
///
/// Fails if the foreign keys (session_id -> repl_sessions.id,
/// previous_id -> session_journals.id) are unsatisfied. Callers
/// are responsible for ensuring the session exists; the normal
/// REPL flow already guarantees this.
pub fn insert_journal(entry: &JournalInsert<'_>) -> Result<i64> {
    let conn = broker::open()?;
    let created = entry.created_at.unwrap_or_else(now_secs);
    conn.execute(
        "INSERT INTO session_journals (
             session_id, project, created_at, from_entry_id,
             to_entry_id, structured_json, headline, previous_id,
             model_used, cred_used, tokens_in, tokens_out
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            entry.session_id,
            entry.project,
            created,
            entry.from_entry_id,
            entry.to_entry_id,
            entry.structured_json,
            entry.headline,
            entry.previous_id,
            entry.model_used,
            entry.cred_used,
            entry.tokens_in,
            entry.tokens_out,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Return up to `limit` most recent journals for a session, newest
/// first. Empty vec if none. Used by session-resume injection and
/// by `/journal` to show recent entries.
pub fn recent_for_session(session_id: &str, limit: usize) -> Result<Vec<JournalRow>> {
    let conn = broker::open()?;
    let mut stmt = conn.prepare(
        "SELECT id, session_id, project, created_at, from_entry_id,
                to_entry_id, structured_json, headline, previous_id,
                model_used, cred_used, tokens_in, tokens_out
           FROM session_journals
          WHERE session_id = ?1
          ORDER BY created_at DESC
          LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![session_id, limit as i64], row_to_journal)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Return up to `limit` most recent journals across all sessions in
/// a project (cwd), newest first. Used by the cross-session memory
/// promoter and the "last seen in this project" startup brief.
pub fn recent_for_project(project: &str, limit: usize) -> Result<Vec<JournalRow>> {
    let conn = broker::open()?;
    let mut stmt = conn.prepare(
        "SELECT id, session_id, project, created_at, from_entry_id,
                to_entry_id, structured_json, headline, previous_id,
                model_used, cred_used, tokens_in, tokens_out
           FROM session_journals
          WHERE project = ?1
          ORDER BY created_at DESC
          LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![project, limit as i64], row_to_journal)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// What was the upper bound (to_entry_id) of the most recent
/// journal for this session? None if no journal has been written
/// yet. The next journaling pass uses this to bound its slice of
/// history — we want `repl_entries.created_at > (returned row's
/// created_at)` essentially, but addressing by id keeps the
/// semantics explicit.
pub fn latest_to_entry_id(session_id: &str) -> Result<Option<String>> {
    let conn = broker::open()?;
    Ok(conn
        .query_row(
            "SELECT to_entry_id FROM session_journals
              WHERE session_id = ?1
              ORDER BY created_at DESC
              LIMIT 1",
            [session_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
}

/// Sum of tokens_in across all journals in a project whose
/// `created_at >= since_unix_secs`. Used by the cost-cap check:
/// before firing a new journaling pass, the caller sums recent
/// spend and skips if it exceeds the configured daily budget.
///
/// Returns 0 if no rows match (nothing spent).
pub fn project_tokens_in_window(project: &str, since_unix_secs: f64) -> Result<i64> {
    let conn = broker::open()?;
    let total: Option<i64> = conn
        .query_row(
            "SELECT SUM(tokens_in) FROM session_journals
              WHERE project = ?1 AND created_at >= ?2",
            params![project, since_unix_secs],
            |r| r.get::<_, Option<i64>>(0),
        )
        .unwrap_or(None);
    Ok(total.unwrap_or(0))
}

/// Load messages for a session as (entry_id, ChatMessage) pairs,
/// optionally bounded to only entries created strictly after the
/// given entry id. Used by the background journaling task: it
/// calls `latest_to_entry_id` to learn the upper bound of the
/// previous pass, then passes that here to grab only the new
/// turns since then.
///
/// Returned pairs are ordered by `created_at ASC` (oldest first),
/// matching the shape `prompt::format_prompt` expects. Entry id
/// is stable — the same value the `repl_entries.id` column holds —
/// so callers can persist `to_entry_id = last returned id` and
/// resume strictly after it on the next pass.
///
/// Why not piggyback on `session::load_history`: that function
/// drops entry ids (returns `Vec<ChatMessage>`), and it loads
/// *all* history. For the journaling pass we need:
///   (a) ids so we can record the bound,
///   (b) only the slice after the last journal,
///   (c) a cheap skip path when there's nothing new (empty Vec).
/// Adding a second loader here rather than widening load_history
/// keeps the hot-path signature unchanged.
pub fn load_slice_after(
    session_id: &str,
    after_entry_id: Option<&str>,
) -> Result<Vec<(String, crate::providers::ChatMessage)>> {
    let conn = broker::open()?;

    // When no previous-journal bound is known, return the full
    // session. The query is parametrized differently in that case:
    // a single SELECT with a guard clause keeps the flow simple
    // and avoids needing two prepare() sites.
    //
    // The `after_entry_id` bound compares `created_at` rather
    // than id-string ordering, because entry ids are UUIDs and
    // UUID lex order has no time correlation. Subquery lookup
    // of the reference row's created_at is a single indexed hit.
    let mut stmt = conn.prepare(
        "SELECT id, role, content FROM repl_entries
          WHERE session_id = ?1 AND entry_type = 'message'
            AND (
                ?2 IS NULL
                OR created_at > (
                    SELECT created_at FROM repl_entries
                     WHERE id = ?2
                     LIMIT 1
                )
            )
          ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map(params![session_id, after_entry_id], |row| {
        let id: String = row.get(0)?;
        let role_str: String = row.get(1)?;
        let content_json: String = row.get(2)?;
        Ok((id, role_str, content_json))
    })?;

    use crate::providers::{ChatMessage, ContentBlock, Role};
    let mut out = Vec::new();
    for r in rows {
        let (id, role_str, content_json) = r?;
        let role = match role_str.as_str() {
            "assistant" => Role::Assistant,
            _ => Role::User,
        };
        let content: Vec<ContentBlock> =
            serde_json::from_str(&content_json).unwrap_or_default();
        out.push((id, ChatMessage { role, content }));
    }
    Ok(out)
}

/// Link a memory_events row to a session_journals row. Idempotent:
/// the composite PK enforces uniqueness, so calling this twice
/// with the same pair is safe — the second INSERT silently
/// no-ops via ON CONFLICT DO NOTHING.
pub fn link_memory_to_journal(memory_id: i64, journal_id: i64) -> Result<()> {
    let conn = broker::open()?;
    conn.execute(
        "INSERT INTO memory_journal_support (memory_id, journal_id, created_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(memory_id, journal_id) DO NOTHING",
        params![memory_id, journal_id, now_secs()],
    )?;
    Ok(())
}

/// How many journals across a project support a given memory?
/// Used by the age-out sweep: memories with zero recent support
/// decay, memories reinforced by many journals stay.
pub fn support_count_for_memory(memory_id: i64) -> Result<i64> {
    let conn = broker::open()?;
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM memory_journal_support WHERE memory_id = ?1",
        [memory_id],
        |r| r.get::<_, i64>(0),
    )?)
}

fn row_to_journal(r: &rusqlite::Row<'_>) -> rusqlite::Result<JournalRow> {
    Ok(JournalRow {
        id: r.get(0)?,
        session_id: r.get(1)?,
        project: r.get(2)?,
        created_at: r.get(3)?,
        from_entry_id: r.get(4)?,
        to_entry_id: r.get(5)?,
        structured_json: r.get(6)?,
        headline: r.get(7)?,
        previous_id: r.get(8)?,
        model_used: r.get(9)?,
        cred_used: r.get(10)?,
        tokens_in: r.get(11)?,
        tokens_out: r.get(12)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker;

    /// Test fixture: isolate HOME and run init_db so schema v2 is
    /// in place. Mirrors src/broker/tests.rs `with_test_db`.
    fn with_test_db<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = crate::test_home_lock()
            .lock()
            .map_err(|_| anyhow::anyhow!("home lock poisoned"))?;
        let old_home = std::env::var_os("HOME");
        let temp = std::env::temp_dir().join(format!(
            "sidekar-journal-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp)?;
        // Safety: in-process test, HOME restored before return.
        unsafe {
            std::env::set_var("HOME", &temp);
        }
        broker::init_db()?;
        let result = f();
        match old_home {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let _ = std::fs::remove_dir_all(&temp);
        result
    }

    /// Seed a repl_sessions row so FK inserts work.
    fn seed_session(id: &str, cwd: &str) -> Result<()> {
        let conn = broker::open()?;
        conn.execute(
            "INSERT INTO repl_sessions (id, cwd, created_at, updated_at)
             VALUES (?1, ?2, 0.0, 0.0)",
            params![id, cwd],
        )?;
        Ok(())
    }

    fn basic_insert<'a>(session_id: &'a str, project: &'a str) -> JournalInsert<'a> {
        JournalInsert {
            session_id,
            project,
            from_entry_id: "e-a",
            to_entry_id: "e-b",
            structured_json: r#"{"summary":"x"}"#,
            headline: "headline-x",
            previous_id: None,
            model_used: "m",
            cred_used: "c",
            tokens_in: 100,
            tokens_out: 25,
            created_at: Some(1_000.0),
        }
    }

    #[test]
    fn insert_and_read_back_roundtrips() -> Result<()> {
        with_test_db(|| {
            seed_session("s-1", "/tmp/p")?;
            let id = insert_journal(&basic_insert("s-1", "/tmp/p"))?;
            assert!(id > 0);

            let rows = recent_for_session("s-1", 10)?;
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].id, id);
            assert_eq!(rows[0].headline, "headline-x");
            assert_eq!(rows[0].tokens_in, 100);
            assert_eq!(rows[0].previous_id, None);
            Ok(())
        })
    }

    #[test]
    fn recent_for_session_orders_newest_first_and_respects_limit() -> Result<()> {
        with_test_db(|| {
            seed_session("s-2", "/tmp/p")?;
            // Insert three with increasing created_at.
            for (i, ts) in [1_000.0, 2_000.0, 3_000.0].iter().enumerate() {
                let mut e = basic_insert("s-2", "/tmp/p");
                e.headline = Box::leak(format!("h-{i}").into_boxed_str());
                e.created_at = Some(*ts);
                insert_journal(&e)?;
            }
            // LIMIT 2 → only the two newest, newest-first order.
            let rows = recent_for_session("s-2", 2)?;
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].headline, "h-2");
            assert_eq!(rows[1].headline, "h-1");
            Ok(())
        })
    }

    #[test]
    fn recent_for_project_spans_sessions() -> Result<()> {
        with_test_db(|| {
            seed_session("s-a", "/tmp/shared")?;
            seed_session("s-b", "/tmp/shared")?;
            seed_session("s-c", "/tmp/other")?;

            let mut a = basic_insert("s-a", "/tmp/shared");
            a.created_at = Some(1_000.0);
            insert_journal(&a)?;
            let mut b = basic_insert("s-b", "/tmp/shared");
            b.created_at = Some(2_000.0);
            insert_journal(&b)?;
            let mut c = basic_insert("s-c", "/tmp/other");
            c.created_at = Some(3_000.0);
            insert_journal(&c)?;

            let shared = recent_for_project("/tmp/shared", 10)?;
            assert_eq!(shared.len(), 2);
            // Newest-first across sessions within the same project.
            assert_eq!(shared[0].session_id, "s-b");
            assert_eq!(shared[1].session_id, "s-a");

            let other = recent_for_project("/tmp/other", 10)?;
            assert_eq!(other.len(), 1);
            assert_eq!(other[0].session_id, "s-c");
            Ok(())
        })
    }

    #[test]
    fn latest_to_entry_id_returns_most_recent() -> Result<()> {
        with_test_db(|| {
            seed_session("s-3", "/tmp/p")?;
            assert_eq!(latest_to_entry_id("s-3")?, None);

            let mut e1 = basic_insert("s-3", "/tmp/p");
            e1.to_entry_id = "id-first";
            e1.created_at = Some(1_000.0);
            insert_journal(&e1)?;

            let mut e2 = basic_insert("s-3", "/tmp/p");
            e2.to_entry_id = "id-second";
            e2.created_at = Some(2_000.0);
            insert_journal(&e2)?;

            assert_eq!(
                latest_to_entry_id("s-3")?,
                Some("id-second".to_string())
            );
            Ok(())
        })
    }

    #[test]
    fn project_tokens_in_window_respects_boundary() -> Result<()> {
        with_test_db(|| {
            seed_session("s-4", "/tmp/p")?;
            // Three journals at t=1000/2000/3000 with 100/200/400 in.
            for (ts, tokens) in [(1_000.0, 100), (2_000.0, 200), (3_000.0, 400)] {
                let mut e = basic_insert("s-4", "/tmp/p");
                e.created_at = Some(ts);
                e.tokens_in = tokens;
                insert_journal(&e)?;
            }
            // since=0 → all three.
            assert_eq!(project_tokens_in_window("/tmp/p", 0.0)?, 700);
            // since=2500 → just the last one.
            assert_eq!(project_tokens_in_window("/tmp/p", 2_500.0)?, 400);
            // since=9999 → nothing, returns 0 not None.
            assert_eq!(project_tokens_in_window("/tmp/p", 9_999.0)?, 0);
            // Unknown project → 0.
            assert_eq!(project_tokens_in_window("/nope", 0.0)?, 0);
            Ok(())
        })
    }

    /// Seed a message row directly. Returns the assigned entry id.
    fn seed_message(
        session_id: &str,
        role: &str,
        text: &str,
        created_at: f64,
    ) -> Result<String> {
        let conn = broker::open()?;
        let id = format!("e-{}-{:x}", role, (created_at * 1_000.0) as u64);
        let content_json = serde_json::to_string(&serde_json::json!([
            {"type": "text", "text": text}
        ]))?;
        conn.execute(
            "INSERT INTO repl_entries (id, session_id, entry_type, role, content, created_at)
             VALUES (?1, ?2, 'message', ?3, ?4, ?5)",
            params![id, session_id, role, content_json, created_at],
        )?;
        Ok(id)
    }

    #[test]
    fn load_slice_after_returns_full_history_when_unbounded() -> Result<()> {
        with_test_db(|| {
            seed_session("s-slice", "/tmp/p")?;
            let _id1 = seed_message("s-slice", "user", "first", 1_000.0)?;
            let _id2 = seed_message("s-slice", "assistant", "second", 2_000.0)?;
            let _id3 = seed_message("s-slice", "user", "third", 3_000.0)?;

            let out = load_slice_after("s-slice", None)?;
            assert_eq!(out.len(), 3);
            // Oldest-first ordering (format_prompt expects this).
            use crate::providers::ContentBlock;
            if let ContentBlock::Text { text } = &out[0].1.content[0] {
                assert_eq!(text, "first");
            }
            if let ContentBlock::Text { text } = &out[2].1.content[0] {
                assert_eq!(text, "third");
            }
            Ok(())
        })
    }

    #[test]
    fn load_slice_after_bounds_strictly_after_reference() -> Result<()> {
        with_test_db(|| {
            seed_session("s-slice-2", "/tmp/p")?;
            let id1 = seed_message("s-slice-2", "user", "first", 1_000.0)?;
            let _id2 = seed_message("s-slice-2", "assistant", "second", 2_000.0)?;
            let _id3 = seed_message("s-slice-2", "user", "third", 3_000.0)?;

            // After id1: expect second + third only.
            let out = load_slice_after("s-slice-2", Some(&id1))?;
            assert_eq!(out.len(), 2);
            use crate::providers::ContentBlock;
            if let ContentBlock::Text { text } = &out[0].1.content[0] {
                assert_eq!(text, "second");
            }
            Ok(())
        })
    }

    #[test]
    fn load_slice_after_empty_when_nothing_new() -> Result<()> {
        with_test_db(|| {
            seed_session("s-slice-3", "/tmp/p")?;
            let id1 = seed_message("s-slice-3", "user", "only one", 1_000.0)?;

            let out = load_slice_after("s-slice-3", Some(&id1))?;
            assert!(out.is_empty());
            Ok(())
        })
    }

    #[test]
    fn load_slice_after_unknown_session_is_empty() -> Result<()> {
        with_test_db(|| {
            let out = load_slice_after("no-such-session", None)?;
            assert!(out.is_empty());
            Ok(())
        })
    }

    #[test]
    fn link_memory_to_journal_is_idempotent() -> Result<()> {
        with_test_db(|| {
            seed_session("s-5", "/tmp/p")?;
            let jid = insert_journal(&basic_insert("s-5", "/tmp/p"))?;

            // Seed a memory row directly (the real memory module
            // has richer helpers; we just need the FK target).
            let conn = broker::open()?;
            conn.execute(
                "INSERT INTO memory_events (
                     project, event_type, scope, summary, summary_norm,
                     confidence, created_at, updated_at
                 ) VALUES ('/tmp/p', 'constraint', 'project',
                           'test', 'test', 0.4, 0, 0)",
                [],
            )?;
            let mid: i64 = conn.query_row(
                "SELECT id FROM memory_events WHERE summary = 'test'",
                [],
                |r| r.get(0),
            )?;

            // First link creates the row.
            link_memory_to_journal(mid, jid)?;
            assert_eq!(support_count_for_memory(mid)?, 1);

            // Second call must not error (ON CONFLICT DO NOTHING)
            // and must not double-count.
            link_memory_to_journal(mid, jid)?;
            assert_eq!(support_count_for_memory(mid)?, 1);
            Ok(())
        })
    }

    #[test]
    fn insert_with_previous_id_chains_iterative_updates() -> Result<()> {
        with_test_db(|| {
            seed_session("s-6", "/tmp/p")?;
            let first = insert_journal(&basic_insert("s-6", "/tmp/p"))?;

            let mut second = basic_insert("s-6", "/tmp/p");
            second.previous_id = Some(first);
            second.created_at = Some(2_000.0);
            let second_id = insert_journal(&second)?;

            let rows = recent_for_session("s-6", 10)?;
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].id, second_id);
            assert_eq!(rows[0].previous_id, Some(first));
            assert_eq!(rows[1].id, first);
            assert_eq!(rows[1].previous_id, None);
            Ok(())
        })
    }
}
