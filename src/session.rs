//! Session persistence — SQLite-backed conversation history with tree structure.

use anyhow::{Context, Result};
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
    pub entry_type: String, // "message", "compaction", "model_change"
    pub role: Option<String>, // "user", "assistant"
    pub content: String,    // JSON blob
    pub created_at: f64,
}

// ---------------------------------------------------------------------------
// Schema migration
// ---------------------------------------------------------------------------

pub fn ensure_tables() -> Result<()> {
    let conn = crate::broker::open()?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS repl_sessions (
            id TEXT PRIMARY KEY,
            cwd TEXT NOT NULL,
            model TEXT NOT NULL DEFAULT '',
            provider TEXT NOT NULL DEFAULT '',
            name TEXT,
            created_at REAL NOT NULL,
            updated_at REAL NOT NULL
        );
        CREATE TABLE IF NOT EXISTS repl_entries (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES repl_sessions(id),
            parent_id TEXT,
            entry_type TEXT NOT NULL DEFAULT 'message',
            role TEXT,
            content TEXT NOT NULL DEFAULT '[]',
            created_at REAL NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_repl_entries_session
            ON repl_entries(session_id, created_at);",
    )
    .context("Failed to create session tables")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Session CRUD
// ---------------------------------------------------------------------------

pub fn create_session(cwd: &str, model: &str, provider: &str) -> Result<String> {
    ensure_tables()?;
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
    ensure_tables()?;
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
    ensure_tables()?;
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
        let content: Vec<ContentBlock> =
            serde_json::from_str(&content_json).unwrap_or_default();
        messages.push(ChatMessage { role, content });
    }
    Ok(messages)
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn generate_id() -> String {
    let mut bytes = [0u8; 12];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn epoch_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
