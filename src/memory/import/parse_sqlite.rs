//! SQLite-backed transcript parsers. Two formats so far:
//!
//! * **Cursor** — `~/.cursor/chats/<workspace>/<chat-id>/store.db`
//!   holds one message per row in a `blobs` table. `blobs.data`
//!   is a JSON object (`role` + `content`) serialized as UTF-8.
//!   Workspace-hash → project path mapping isn't stored anywhere
//!   we can reach from Rust, so we fall back to scanning the
//!   first user message's `<user_info>` block which usually
//!   contains `Workspace Path: <cwd>`.
//!
//! * **Opencode** — single `~/.local/share/opencode/opencode.db`
//!   with `session`, `message`, `part` tables. Straight relational
//!   join; `session.directory` gives the cwd directly.

use super::parse_transcripts::{SessionTranscript, Turn};
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Parse a single Cursor `store.db`. Returns one `SessionTranscript`
/// covering every message in the DB — one Cursor chat == one
/// session, which matches our scoping model.
pub(super) fn parse_cursor_store_db(path: &Path) -> Result<SessionTranscript> {
    let conn = open_readonly(path)?;
    let mut stmt = conn
        .prepare("SELECT data FROM blobs ORDER BY rowid ASC")
        .with_context(|| format!("prepare {}", path.display()))?;
    let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
    let mut turns = Vec::new();
    let mut cwd: Option<PathBuf> = None;

    for row in rows {
        let bytes = match row {
            Ok(b) => b,
            Err(_) => continue,
        };
        let text = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let value: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let role = value.get("role").and_then(Value::as_str).unwrap_or("");
        if !matches!(role, "user" | "assistant") {
            continue;
        }
        let content = match value.get("content") {
            Some(c) => c,
            None => continue,
        };
        let message_text = flatten_cursor_content(content);
        if message_text.trim().is_empty() {
            continue;
        }
        if cwd.is_none() && role == "user" {
            cwd = extract_cursor_workspace_path(&message_text).map(PathBuf::from);
        }
        turns.push(Turn {
            role: role.to_string(),
            text: strip_cursor_user_info(&message_text),
        });
    }

    Ok(SessionTranscript {
        source_path: path.to_path_buf(),
        cwd,
        turns,
    })
}

fn flatten_cursor_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            let mut parts = Vec::new();
            for item in items {
                let Some(obj) = item.as_object() else {
                    continue;
                };
                let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
                match kind {
                    "text" => {
                        if let Some(t) = obj.get("text").and_then(Value::as_str) {
                            parts.push(t.to_string());
                        }
                    }
                    // Tool results are mostly noise; skip.
                    _ => {}
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

/// Cursor appends a `<user_info>...</user_info>` block to the first
/// user message containing things like `Workspace Path: ...`,
/// `OS Version: ...`, etc. Pull the workspace path if present so we
/// can attribute the chat to the right project.
fn extract_cursor_workspace_path(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Workspace Path:") {
            let path = rest.trim();
            if !path.is_empty() {
                return Some(path.to_string());
            }
        }
    }
    None
}

/// Strip the `<user_info>...</user_info>` prelude from a user
/// message so the LLM doesn't get distracted by boilerplate. Only
/// removes the first such block; real user content after it is
/// preserved.
fn strip_cursor_user_info(text: &str) -> String {
    if let Some(start) = text.find("<user_info>")
        && let Some(end_rel) = text[start..].find("</user_info>")
    {
        let end = start + end_rel + "</user_info>".len();
        let mut out = String::new();
        out.push_str(&text[..start]);
        out.push_str(&text[end..]);
        return out.trim().to_string();
    }
    text.to_string()
}

// ---- Opencode -------------------------------------------------------------

/// Each Opencode session becomes a single SessionTranscript. Per-
/// message `data` is JSON; per-part `data` is JSON describing text
/// content / tool calls / etc. We only keep `text` parts from user
/// and assistant messages.
pub(super) fn parse_opencode_db(path: &Path) -> Result<Vec<SessionTranscript>> {
    let conn = open_readonly(path)?;
    let sessions = list_opencode_sessions(&conn)?;
    let mut out = Vec::new();
    for sess in sessions {
        let turns = load_opencode_turns(&conn, &sess.id)?;
        if turns.is_empty() {
            continue;
        }
        out.push(SessionTranscript {
            source_path: path.to_path_buf(),
            cwd: sess.directory.map(PathBuf::from),
            turns,
        });
    }
    Ok(out)
}

struct OpencodeSession {
    id: String,
    directory: Option<String>,
}

fn list_opencode_sessions(conn: &Connection) -> Result<Vec<OpencodeSession>> {
    // Pull every session ordered most-recent first so the
    // --max-sessions cap in the caller keeps the useful ones.
    // Some test fixtures omit time_created — fall back gracefully.
    let query_ordered =
        "SELECT id, directory FROM session ORDER BY COALESCE(time_created, 0) DESC";
    let query_plain = "SELECT id, directory FROM session";
    let mut stmt = match conn.prepare(query_ordered) {
        Ok(s) => s,
        Err(_) => conn
            .prepare(query_plain)
            .context("prepare session query")?,
    };
    let rows = stmt.query_map([], |r| {
        Ok(OpencodeSession {
            id: r.get::<_, String>(0)?,
            directory: r.get::<_, Option<String>>(1)?,
        })
    })?;
    Ok(rows.flatten().collect())
}

fn load_opencode_turns(conn: &Connection, session_id: &str) -> Result<Vec<Turn>> {
    // message.data carries role; part.data carries content. Join
    // on part.message_id = message.id.
    // Opencode's schema uses time_created, not created_at. Verified
    // by dumping the live DB's schema — message and part both have
    // (id, session_id, time_created, time_updated, data).
    let mut stmt = conn
        .prepare(
            "SELECT m.data, p.data
             FROM message m
             LEFT JOIN part p ON p.message_id = m.id
             WHERE m.session_id = ?1
             ORDER BY m.time_created ASC, p.time_created ASC",
        )
        .context("prepare message+part query")?;
    let rows = stmt.query_map(params![session_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
        ))
    })?;

    let mut turns = Vec::new();
    for row in rows.flatten() {
        let (msg_json, part_json) = row;
        let msg: Value = match serde_json::from_str(&msg_json) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let role = msg
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !matches!(role.as_str(), "user" | "assistant") {
            continue;
        }
        let Some(part_json) = part_json else { continue };
        let part: Value = match serde_json::from_str(&part_json) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if part.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        let text = part
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if text.trim().is_empty() {
            continue;
        }
        // Merge consecutive parts from the same message into one turn.
        if let Some(last) = turns.last_mut() {
            let last: &mut Turn = last;
            if last.role == role {
                last.text.push('\n');
                last.text.push_str(&text);
                continue;
            }
        }
        turns.push(Turn { role, text });
    }
    Ok(turns)
}

fn open_readonly(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn tmp_db(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("sidekar-sqlite-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir.join(name)
    }

    #[test]
    fn cursor_blobs_produce_turns_and_workspace_path() {
        let path = tmp_db("cursor.db");
        let _ = std::fs::remove_file(&path);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE blobs (id INTEGER PRIMARY KEY, data BLOB);",
        )
        .unwrap();
        let user = r#"{"role":"user","content":"<user_info>\nOS Version: darwin\nWorkspace Path: /Users/me/demo\n</user_info>\n\nhi there"}"#;
        let assistant = r#"{"role":"assistant","content":[{"type":"text","text":"hello"},{"type":"tool-result","result":"..."}]}"#;
        let tool = r#"{"role":"tool","content":[]}"#;
        for body in &[user, assistant, tool] {
            conn.execute(
                "INSERT INTO blobs (data) VALUES (?1)",
                params![body.as_bytes()],
            )
            .unwrap();
        }
        drop(conn);

        let t = parse_cursor_store_db(&path).unwrap();
        assert_eq!(t.cwd.as_deref(), Some(Path::new("/Users/me/demo")));
        assert_eq!(t.turns.len(), 2);
        assert_eq!(t.turns[0].role, "user");
        // user_info block stripped, "hi there" preserved.
        assert!(t.turns[0].text.contains("hi there"));
        assert!(!t.turns[0].text.contains("Workspace Path"));
        assert_eq!(t.turns[1].role, "assistant");
        assert_eq!(t.turns[1].text, "hello");
    }

    #[test]
    fn cursor_parser_handles_missing_workspace_path() {
        let path = tmp_db("cursor-nowork.db");
        let _ = std::fs::remove_file(&path);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE blobs (id INTEGER PRIMARY KEY, data BLOB);",
        )
        .unwrap();
        let user = r#"{"role":"user","content":"just a question"}"#;
        conn.execute(
            "INSERT INTO blobs (data) VALUES (?1)",
            params![user.as_bytes()],
        )
        .unwrap();
        drop(conn);

        let t = parse_cursor_store_db(&path).unwrap();
        assert!(t.cwd.is_none());
        assert_eq!(t.turns.len(), 1);
    }

    #[test]
    fn opencode_joins_session_message_and_part() {
        let path = tmp_db("opencode.db");
        let _ = std::fs::remove_file(&path);
        let conn = Connection::open(&path).unwrap();
        // Use time_created to match the real Opencode schema; our
        // query also has a plain-query fallback so older DBs work.
        conn.execute_batch(
            "\
            CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, time_created INTEGER, data TEXT);
            CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
            CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, time_created INTEGER, data TEXT);
            ",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, directory, time_created, data) VALUES ('s1', '/Users/me/proj', 100, '{}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, data) VALUES ('m1', 's1', 1, '{\"role\":\"user\"}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part (id, message_id, time_created, data) VALUES ('p1', 'm1', 1, '{\"type\":\"text\",\"text\":\"hi\"}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, data) VALUES ('m2', 's1', 2, '{\"role\":\"assistant\"}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part (id, message_id, time_created, data) VALUES ('p2', 'm2', 2, '{\"type\":\"text\",\"text\":\"hello\"}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part (id, message_id, time_created, data) VALUES ('p3', 'm2', 3, '{\"type\":\"tool-call\"}')",
            [],
        )
        .unwrap();
        drop(conn);

        let transcripts = parse_opencode_db(&path).unwrap();
        assert_eq!(transcripts.len(), 1);
        let t = &transcripts[0];
        assert_eq!(t.cwd.as_deref(), Some(Path::new("/Users/me/proj")));
        assert_eq!(t.turns.len(), 2);
        assert_eq!(t.turns[0].role, "user");
        assert_eq!(t.turns[0].text, "hi");
        assert_eq!(t.turns[1].role, "assistant");
        assert_eq!(t.turns[1].text, "hello");
    }

    #[test]
    fn opencode_empty_session_omitted() {
        let path = tmp_db("opencode-empty.db");
        let _ = std::fs::remove_file(&path);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "\
            CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, time_created INTEGER, data TEXT);
            CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
            CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, time_created INTEGER, data TEXT);
            ",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, directory, time_created, data) VALUES ('s1', '/proj', 1, '{}')",
            [],
        )
        .unwrap();
        drop(conn);
        let transcripts = parse_opencode_db(&path).unwrap();
        assert!(transcripts.is_empty());
    }
}
