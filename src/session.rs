//! Session persistence — SQLite-backed conversation history with tree structure.

use anyhow::Result;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::providers::{ChatMessage, ContentBlock, Role};

#[allow(dead_code)]
const SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Session and entry types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub cwd: String,
    pub model: String,
    /// Credential nickname that created this session (e.g. "claude",
    /// "codex-work"). Empty string for legacy rows. Column is named
    /// `provider` in SQL for historical reasons.
    pub provider: String,
    pub name: Option<String>,
    pub created_at: f64,
    pub updated_at: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    pub id: String,
    pub session_id: String,
    pub parent_id: Option<String>,
    pub entry_type: String,   // "message", "compaction", "model_change"
    pub role: Option<String>, // "user", "assistant"
    pub content: String,      // JSON blob
    pub created_at: f64,
}

// ---------------------------------------------------------------------------
// Session CRUD
// ---------------------------------------------------------------------------
//
// Schema lives in `broker::init_schema` and is created once per process on
// the first `broker::open()` call. Do not re-run `CREATE TABLE` here — it
// would reinstate the per-call overhead this module used to pay.

pub fn create_session(cwd: &str, model: &str, provider: &str) -> Result<String> {
    let id = generate_id();
    let now = epoch_secs();
    let conn = crate::broker::open()?;
    conn.execute(
        "INSERT INTO repl_sessions (id, cwd, model, provider, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        rusqlite::params![id, cwd, model, provider, now],
    )?;
    Ok(id)
}

pub fn update_session_time(session_id: &str) -> Result<()> {
    let conn = crate::broker::open()?;
    let now = epoch_secs();
    conn.execute(
        "UPDATE repl_sessions SET updated_at = ?1 WHERE id = ?2",
        rusqlite::params![now, session_id],
    )?;
    Ok(())
}

/// List sessions for the current working directory, most recent first.
pub fn list_sessions(cwd: &str, limit: usize) -> Result<Vec<Session>> {
    let conn = crate::broker::open()?;
    let mut stmt = conn.prepare(
        "SELECT id, cwd, model, provider, name, created_at, updated_at
         FROM repl_sessions WHERE cwd = ?1
         ORDER BY updated_at DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![cwd, limit], |row| {
        Ok(Session {
            id: row.get(0)?,
            cwd: row.get(1)?,
            model: row.get(2)?,
            provider: row.get(3)?,
            name: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    })?;
    let mut sessions = Vec::new();
    for row in rows {
        sessions.push(row?);
    }
    Ok(sessions)
}

/// Session paired with its message count and optional last-user-
/// prompt JSON.
///
/// Returned by [`list_sessions_with_counts`] so callers that want to
/// display or filter by "has the user actually said anything in this
/// session" don't need a follow-up `message_count` per row — the
/// count is computed in the same SQL round-trip via a correlated
/// subquery.
///
/// `last_user_content_json` is `Some(raw_content_blocks_json)` for
/// the most recent user message (chronologically) in the session, or
/// `None` if the session has no user messages yet. Kept as raw JSON
/// so the caller decides how to render — the common case
/// (`last_prompt_snippet`) extracts the first text block and
/// truncates; other callers might want the full content.
#[derive(Debug, Clone)]
pub struct SessionWithCount {
    pub session: Session,
    pub messages: usize,
    pub last_user_content_json: Option<String>,
}

impl SessionWithCount {
    /// Extract a short preview of the most recent user prompt,
    /// truncated to `max_chars` grapheme-aware chars. Returns `None`
    /// if no user message exists or the content has no text block.
    ///
    /// "Prompt" here means the user-role content; tool results and
    /// images are ignored (tool results are noisy, images have no
    /// useful preview). We take the FIRST text block of the LAST
    /// user message: multi-block messages usually put the prompt
    /// text first (then pasted files / images follow), so this
    /// yields the intended title-like preview.
    ///
    /// The truncation adds an ellipsis when it trims. Newlines are
    /// collapsed to single spaces so the preview stays on one line.
    pub fn last_prompt_snippet(&self, max_chars: usize) -> Option<String> {
        let json = self.last_user_content_json.as_ref()?;
        let blocks: Vec<ContentBlock> = serde_json::from_str(json).ok()?;
        let text = blocks.iter().find_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })?;
        let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if flattened.is_empty() {
            return None;
        }
        // char_indices gives us UTF-8-safe truncation boundaries.
        // Counting chars, not bytes, matches the caller's intent
        // ("first 30 chars").
        let char_count = flattened.chars().count();
        if char_count <= max_chars {
            Some(flattened)
        } else {
            let end = flattened
                .char_indices()
                .nth(max_chars)
                .map(|(i, _)| i)
                .unwrap_or(flattened.len());
            let mut truncated = flattened[..end].to_string();
            truncated.push('…');
            Some(truncated)
        }
    }
}

/// List sessions for the current working directory, most recent
/// first, each annotated with its message count.
///
/// If `only_nonempty` is true, SQL filters out 0-message sessions at
/// the query layer — so `limit` is applied against the populated
/// rows, not wasted on empty ones. That's important when a user has
/// many `/new` sessions from accidental resets: filtering client-
/// side would return ≤limit total rows (some empty, some not), then
/// drop the empties, leaving fewer real results than the caller
/// asked for.
///
/// The correlated subquery counts rows in `repl_entries` of type
/// "message". Cheap because `repl_entries.session_id` is indexed
/// (see `broker::init_schema`). For limit=10 this runs ~10 sub-
/// queries, each an indexed scan bounded by that session's entry
/// count; total cost is well under the fork+exec of a single
/// external process call.
pub fn list_sessions_with_counts(
    cwd: &str,
    limit: usize,
    only_nonempty: bool,
) -> Result<Vec<SessionWithCount>> {
    let conn = crate::broker::open()?;
    // SQLite doesn't let the WHERE clause reference a SELECT-list
    // alias, so the correlated subquery is repeated in the WHERE
    // when filtering. The query planner folds the two subqueries
    // into one evaluation per row, so the cost is the same as
    // computing msg_count once.
    //
    // `only_nonempty` is passed as 0/1 rather than branching between
    // two prepared statements, to keep this function a single cache
    // slot in rusqlite's prepared-statement cache.
    // Three correlated subqueries per row: message count, message-
    // count-for-filter (repeated because SQLite WHERE can't see
    // SELECT-list aliases), and the last user prompt's content JSON.
    // All three hit the same (session_id, entry_type, role) shape on
    // an indexed column, so the planner shares scans where possible.
    // For a LIMIT of 20-50 this is well under a millisecond on any
    // reasonable session history.
    let sql = "SELECT
            s.id, s.cwd, s.model, s.provider, s.name,
            s.created_at, s.updated_at,
            (SELECT COUNT(*) FROM repl_entries
               WHERE session_id = s.id AND entry_type = 'message') AS msg_count,
            (SELECT content FROM repl_entries
               WHERE session_id = s.id
                 AND entry_type = 'message'
                 AND role = 'user'
               ORDER BY created_at DESC LIMIT 1) AS last_user_content
          FROM repl_sessions s
          WHERE s.cwd = ?1
            AND (?2 = 0 OR (SELECT COUNT(*) FROM repl_entries
                              WHERE session_id = s.id
                                AND entry_type = 'message') > 0)
          ORDER BY s.updated_at DESC
          LIMIT ?3";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(rusqlite::params![cwd, only_nonempty as i64, limit], |row| {
        let msg_count: i64 = row.get(7)?;
        let last_user_content_json: Option<String> = row.get(8)?;
        Ok(SessionWithCount {
            session: Session {
                id: row.get(0)?,
                cwd: row.get(1)?,
                model: row.get(2)?,
                provider: row.get(3)?,
                name: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            },
            messages: msg_count.max(0) as usize,
            last_user_content_json,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// List sessions across all directories.
pub fn list_all_sessions(limit: usize) -> Result<Vec<Session>> {
    let conn = crate::broker::open()?;
    let mut stmt = conn.prepare(
        "SELECT id, cwd, model, provider, name, created_at, updated_at
         FROM repl_sessions
         ORDER BY updated_at DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![limit], |row| {
        Ok(Session {
            id: row.get(0)?,
            cwd: row.get(1)?,
            model: row.get(2)?,
            provider: row.get(3)?,
            name: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    })?;
    let mut sessions = Vec::new();
    for row in rows {
        sessions.push(row?);
    }
    Ok(sessions)
}

/// Find a session by ID prefix match (across all cwds).
pub fn find_session_by_prefix(prefix: &str) -> Result<Option<Session>> {
    let conn = crate::broker::open()?;
    let pattern = format!("{prefix}%");
    let mut stmt = conn.prepare(
        "SELECT id, cwd, model, provider, name, created_at, updated_at
         FROM repl_sessions WHERE id LIKE ?1
         ORDER BY updated_at DESC LIMIT 1",
    )?;
    stmt.query_row(rusqlite::params![pattern], |row| {
        Ok(Session {
            id: row.get(0)?,
            cwd: row.get(1)?,
            model: row.get(2)?,
            provider: row.get(3)?,
            name: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    })
    .optional()
    .map_err(Into::into)
}

/// Get the most recent session for the current cwd.
pub fn latest_session(cwd: &str) -> Result<Option<Session>> {
    let sessions = list_sessions(cwd, 1)?;
    Ok(sessions.into_iter().next())
}

// ---------------------------------------------------------------------------
// Entry CRUD
// ---------------------------------------------------------------------------

pub fn append_entry(
    session_id: &str,
    parent_id: Option<&str>,
    entry_type: &str,
    role: Option<&str>,
    content: &[ContentBlock],
) -> Result<String> {
    let id = generate_id();
    let now = epoch_secs();
    let content_json = serde_json::to_string(content)?;
    let conn = crate::broker::open()?;
    conn.execute(
        "INSERT INTO repl_entries (id, session_id, parent_id, entry_type, role, content, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![id, session_id, parent_id, entry_type, role, content_json, now],
    )?;
    update_session_time(session_id)?;
    Ok(id)
}

pub fn append_message(session_id: &str, msg: &ChatMessage) -> Result<String> {
    let role = match msg.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    append_entry(session_id, None, "message", Some(role), &msg.content)
}

/// Load all messages for a session in chronological order.
pub fn load_history(session_id: &str) -> Result<Vec<ChatMessage>> {
    let conn = crate::broker::open()?;
    let mut stmt = conn.prepare(
        "SELECT role, content FROM repl_entries
         WHERE session_id = ?1 AND entry_type = 'message'
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![session_id], |row| {
        let role_str: String = row.get(0)?;
        let content_json: String = row.get(1)?;
        Ok((role_str, content_json))
    })?;

    let mut messages = Vec::new();
    for row in rows {
        let (role_str, content_json) = row?;
        let role = match role_str.as_str() {
            "assistant" => Role::Assistant,
            _ => Role::User,
        };
        let content: Vec<ContentBlock> = serde_json::from_str(&content_json).unwrap_or_default();
        messages.push(ChatMessage { role, content });
    }
    Ok(messages)
}

/// Replace all messages in a session (used after compaction).
pub fn replace_history(session_id: &str, messages: &[ChatMessage]) -> Result<()> {
    let conn = crate::broker::open()?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM repl_entries WHERE session_id = ?1 AND entry_type = 'message'",
        rusqlite::params![session_id],
    )?;
    let now = epoch_secs();
    for msg in messages {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        let content_json = serde_json::to_string(&msg.content).unwrap_or_else(|_| "[]".into());
        let id = generate_id();
        tx.execute(
            "INSERT INTO repl_entries (id, session_id, entry_type, role, content, created_at)
             VALUES (?1, ?2, 'message', ?3, ?4, ?5)",
            rusqlite::params![id, session_id, role, content_json, now],
        )?;
    }
    tx.execute(
        "UPDATE repl_sessions SET updated_at = ?1 WHERE id = ?2",
        rusqlite::params![now, session_id],
    )?;
    tx.commit()?;
    Ok(())
}

/// Count messages in a session.
pub fn message_count(session_id: &str) -> Result<usize> {
    let conn = crate::broker::open()?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM repl_entries WHERE session_id = ?1 AND entry_type = 'message'",
        rusqlite::params![session_id],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// Delete a session and its entries.
pub fn delete_session(session_id: &str) -> Result<()> {
    let conn = crate::broker::open()?;
    conn.execute(
        "DELETE FROM repl_entries WHERE session_id = ?1",
        rusqlite::params![session_id],
    )?;
    conn.execute(
        "DELETE FROM repl_sessions WHERE id = ?1",
        rusqlite::params![session_id],
    )?;
    Ok(())
}

/// Delete all sessions with zero messages. Returns count of pruned sessions.
pub fn prune_empty_sessions() -> Result<usize> {
    let conn = crate::broker::open()?;
    // Find empty sessions (no message entries)
    let mut stmt = conn.prepare(
        "SELECT s.id FROM repl_sessions s
         WHERE NOT EXISTS (
             SELECT 1 FROM repl_entries e
             WHERE e.session_id = s.id AND e.entry_type = 'message'
         )",
    )?;
    let ids: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    let count = ids.len();
    for id in &ids {
        let _ = conn.execute(
            "DELETE FROM repl_entries WHERE session_id = ?1",
            rusqlite::params![id],
        );
        let _ = conn.execute(
            "DELETE FROM repl_sessions WHERE id = ?1",
            rusqlite::params![id],
        );
    }
    Ok(count)
}

// ---------------------------------------------------------------------------
// REPL input history
// ---------------------------------------------------------------------------

pub fn load_input_history(scope_root: &str, limit: usize) -> Result<Vec<String>> {
    let conn = crate::broker::open()?;
    let mut stmt = conn.prepare(
        "SELECT line FROM repl_input_history
         WHERE scope_root = ?1
         ORDER BY id DESC
         LIMIT ?2",
    )?;
    let mut lines: Vec<String> = stmt
        .query_map(rusqlite::params![scope_root, limit], |row| row.get(0))?
        .filter_map(|row| row.ok())
        .collect();
    lines.reverse();
    Ok(lines)
}

pub fn append_input_history(
    scope_root: &str,
    scope_name: &str,
    line: &str,
    max_entries: usize,
) -> Result<()> {
    if line.trim().is_empty() {
        return Ok(());
    }

    let conn = crate::broker::open()?;
    let tx = conn.unchecked_transaction()?;

    let previous: Option<String> = tx
        .query_row(
            "SELECT line FROM repl_input_history
             WHERE scope_root = ?1
             ORDER BY id DESC
             LIMIT 1",
            rusqlite::params![scope_root],
            |row| row.get(0),
        )
        .optional()?;
    if previous.as_deref() != Some(line) {
        tx.execute(
            "INSERT INTO repl_input_history (scope_root, scope_name, line, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![scope_root, scope_name, line, epoch_secs()],
        )?;
    }

    if max_entries > 0 {
        tx.execute(
            "DELETE FROM repl_input_history
             WHERE scope_root = ?1
               AND id NOT IN (
                   SELECT id FROM repl_input_history
                   WHERE scope_root = ?1
                   ORDER BY id DESC
                   LIMIT ?2
               )",
            rusqlite::params![scope_root, max_entries],
        )?;
    }

    tx.commit()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn generate_id() -> String {
    let mut bytes = [0u8; 12];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Format a unix-seconds timestamp as a short relative age (e.g.
/// "3s", "12m", "4h", "2d", "3w"). Intended for compact listings
/// like `/session` where absolute timestamps are noise. Always one
/// unit: we pick the coarsest that keeps the value <60 (<24 for
/// hours, <7 for days).
///
/// For future timestamps (clock skew, or a caller passing created_at
/// from a DB row that's in the future relative to local time), we
/// just return "now" rather than "-3s" — relative-past phrasing
/// doesn't make sense for future values and the UI caller has no
/// sensible alternative anyway.
pub fn format_relative_age(past_unix_secs: f64, now_unix_secs: f64) -> String {
    let delta = (now_unix_secs - past_unix_secs).max(0.0);
    let secs = delta as u64;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 60 * 60 {
        format!("{}m", secs / 60)
    } else if secs < 60 * 60 * 24 {
        format!("{}h", secs / 3600)
    } else if secs < 60 * 60 * 24 * 7 {
        format!("{}d", secs / 86_400)
    } else if secs < 60 * 60 * 24 * 30 {
        format!("{}w", secs / (86_400 * 7))
    } else {
        // Past a month, pretend it's always "30d+" — months/years
        // are irrelevant to a sessions-to-switch-to picker.
        "30d+".to_string()
    }
}

fn epoch_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[cfg(test)]
mod tests;
