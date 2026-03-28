//! SQLite-backed broker state for sidekar agent coordination.
//!
//! This module is intentionally narrow: it persists agent registrations,
//! pending inbound envelopes, and outbound request tracking so the bus can
//! provide durable state for bus coordination.

use crate::message::{AgentId, Envelope};
use crate::*;
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::Engine;
use rand::Rng;
use rusqlite::{params, Connection, OptionalExtension};

const DB_FILE: &str = "broker.sqlite3";

#[derive(Debug, Clone)]
pub struct BrokerAgent {
    pub id: AgentId,
    pub pane_unique_id: Option<String>,
    pub socket_path: Option<String>,
    pub cwd: Option<String>,
    pub registered_at: u64,
    pub last_seen_at: u64,
}

#[derive(Debug, Clone)]
pub struct OutboundRequestRecord {
    pub msg_id: String,
    pub sender_name: String,
    pub sender_label: String,
    pub recipient_name: String,
    pub transport_name: String,
    pub transport_target: String,
    pub created_at: u64,
    pub nudge_count: u32,
}

fn data_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".sidekar")
}

pub fn db_path() -> PathBuf {
    data_dir().join(DB_FILE)
}

/// Open the broker SQLite database (creating it + schema if needed).
pub fn open_db() -> Result<Connection> {
    open()
}

fn open() -> Result<Connection> {
    fs::create_dir_all(data_dir())?;
    let path = db_path();
    let conn = Connection::open(&path)
        .with_context(|| format!("failed to open broker database at {}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    init_schema(&conn)?;
    Ok(conn)
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS agents (
            name TEXT PRIMARY KEY,
            nick TEXT,
            session TEXT,
            pane TEXT,
            pane_unique_id TEXT,
            agent_type TEXT,
            socket_path TEXT,
            cwd TEXT,
            registered_at INTEGER NOT NULL,
            last_seen_at INTEGER NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_agents_pane_unique
            ON agents(pane_unique_id)
            WHERE pane_unique_id IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_agents_session
            ON agents(session);
        CREATE INDEX IF NOT EXISTS idx_agents_nick
            ON agents(nick);

        CREATE TABLE IF NOT EXISTS pending_messages (
            id TEXT PRIMARY KEY,
            recipient_name TEXT NOT NULL,
            envelope_json TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_pending_recipient
            ON pending_messages(recipient_name, created_at);

        CREATE TABLE IF NOT EXISTS outbound_requests (
            msg_id TEXT PRIMARY KEY,
            sender_name TEXT NOT NULL,
            sender_label TEXT NOT NULL,
            recipient_name TEXT NOT NULL,
            transport_name TEXT NOT NULL,
            transport_target TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            nudge_count INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_outbound_sender
            ON outbound_requests(sender_name, created_at);
        ",
    )?;
    // Migration: add cwd column if missing (existing databases)
    let _ = conn.execute_batch("ALTER TABLE agents ADD COLUMN cwd TEXT");

    // Bus message queue — replaces IPC sockets for agent-to-agent delivery.
    // Writer inserts a row, recipient's poller reads and deletes it.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS bus_queue (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            recipient TEXT NOT NULL,
            sender TEXT NOT NULL,
            body TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_bus_queue_recipient
            ON bus_queue(recipient, id);
        ",
    )?;

    // Cron jobs table
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS cron_jobs (
            id TEXT PRIMARY KEY,
            name TEXT,
            schedule TEXT NOT NULL,
            action_json TEXT NOT NULL,
            target TEXT NOT NULL,
            created_by TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            last_run_at INTEGER,
            run_count INTEGER NOT NULL DEFAULT 0,
            error_count INTEGER NOT NULL DEFAULT 0,
            last_error TEXT,
            active INTEGER NOT NULL DEFAULT 1
        );
        CREATE INDEX IF NOT EXISTS idx_cron_active
            ON cron_jobs(active);
        ",
    )?;

    // TOTP secrets table
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS totp_secrets (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            service TEXT NOT NULL,
            account TEXT NOT NULL,
            secret TEXT NOT NULL,
            algorithm TEXT NOT NULL DEFAULT 'SHA1',
            digits INTEGER NOT NULL DEFAULT 6,
            period INTEGER NOT NULL DEFAULT 30,
            created_at INTEGER NOT NULL,
            UNIQUE(service, account)
        );
        CREATE INDEX IF NOT EXISTS idx_totp_service
            ON totp_secrets(service);
        ",
    )?;

    // KV store table (per-project + global) — project = cwd path
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS kv_store (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            scope TEXT,
            key TEXT NOT NULL,
            value TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(scope, key)
        );
        CREATE INDEX IF NOT EXISTS idx_kv_scope
            ON kv_store(scope);
        CREATE INDEX IF NOT EXISTS idx_kv_global
            ON kv_store(scope, key) WHERE scope IS NULL;
        ",
    )?;

    // Migration: add user_id to kv_store and totp_secrets for multi-account isolation
    let _ = conn.execute_batch("ALTER TABLE kv_store ADD COLUMN user_id TEXT");
    let _ = conn.execute_batch("ALTER TABLE totp_secrets ADD COLUMN user_id TEXT");
    // Migration: rename agent_id → scope in kv_store
    let _ = conn.execute_batch("ALTER TABLE kv_store RENAME COLUMN agent_id TO scope");
    // New unique constraints including user_id (old constraint stays, new one takes priority)
    let _ = conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_kv_user_agent_key ON kv_store(user_id, agent_id, key);
         CREATE UNIQUE INDEX IF NOT EXISTS idx_totp_user_service_account ON totp_secrets(user_id, service, account);
         CREATE UNIQUE INDEX IF NOT EXISTS idx_kv_user_scope_key ON kv_store(user_id, scope, key);",
    );

    // Encryption key marker
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS encryption_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        ",
    )?;

    // Config key-value store (replaces ~/.config/sidekar/sidekar.json)
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS config (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        ",
    )?;

    // Local error log (durable, queryable; ~/.sidekar/broker.sqlite3)
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS error_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at INTEGER NOT NULL,
            source TEXT NOT NULL,
            message TEXT NOT NULL,
            details TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_error_events_created
            ON error_events(created_at);
        ",
    )?;

    Ok(())
}

pub fn register_agent(agent: &AgentId, pane_unique_id: Option<&str>) -> Result<()> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    let pane = agent.pane.as_deref();
    let session = agent.session.as_deref();
    let nick = agent.nick.as_deref();
    let agent_type = agent.agent_type.as_deref();
    let pane_unique_id = pane_unique_id.map(str::to_string);
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM agents WHERE name = ?1", params![agent.name])?;
    if let Some(ref unique) = pane_unique_id {
        tx.execute(
            "DELETE FROM agents WHERE pane_unique_id = ?1",
            params![unique],
        )?;
    }
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().to_string());
    tx.execute(
        "INSERT INTO agents (
            name, nick, session, pane, pane_unique_id, agent_type, socket_path, cwd, registered_at, last_seen_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?8)",
        params![
            agent.name,
            nick,
            session,
            pane,
            pane_unique_id,
            agent_type,
            cwd,
            now,
        ],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn set_agent_socket_path(name: &str, socket_path: Option<&Path>) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "UPDATE agents SET socket_path = ?2, last_seen_at = ?3 WHERE name = ?1",
        params![
            name,
            socket_path.map(|p| p.to_string_lossy().to_string()),
            crate::message::epoch_secs() as i64
        ],
    )?;
    Ok(())
}

pub fn touch_agent(name: &str) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "UPDATE agents SET last_seen_at = ?2 WHERE name = ?1",
        params![name, crate::message::epoch_secs() as i64],
    )?;
    Ok(())
}

pub fn unregister_agent(name: &str) -> Result<()> {
    let conn = open()?;
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM agents WHERE name = ?1", params![name])?;
    tx.execute(
        "DELETE FROM pending_messages WHERE recipient_name = ?1",
        params![name],
    )?;
    tx.execute(
        "DELETE FROM outbound_requests WHERE sender_name = ?1",
        params![name],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn agent_for_pane_unique(pane_unique_id: &str) -> Result<Option<BrokerAgent>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT name, nick, session, pane, pane_unique_id, agent_type, socket_path, cwd, registered_at, last_seen_at
         FROM agents
         WHERE pane_unique_id = ?1
         LIMIT 1",
    )?;
    stmt.query_row(params![pane_unique_id], row_to_agent)
        .optional()
        .map_err(Into::into)
}

pub fn list_agents(session: Option<&str>) -> Result<Vec<BrokerAgent>> {
    let conn = open()?;
    let sql = if session.is_some() {
        "SELECT name, nick, session, pane, pane_unique_id, agent_type, socket_path, cwd, registered_at, last_seen_at
         FROM agents
         WHERE session = ?1
         ORDER BY name"
    } else {
        "SELECT name, nick, session, pane, pane_unique_id, agent_type, socket_path, cwd, registered_at, last_seen_at
         FROM agents
         ORDER BY name"
    };
    let mut stmt = conn.prepare(sql)?;
    let mut rows = if let Some(session) = session {
        stmt.query(params![session])?
    } else {
        stmt.query([])?
    };
    let mut agents = Vec::new();
    while let Some(row) = rows.next()? {
        agents.push(row_to_agent(row)?);
    }
    Ok(agents)
}

pub fn find_agent(target: &str, session: Option<&str>) -> Result<Option<BrokerAgent>> {
    let conn = open()?;
    let mut stmt = if session.is_some() {
        conn.prepare(
            "SELECT name, nick, session, pane, pane_unique_id, agent_type, socket_path, cwd, registered_at, last_seen_at
             FROM agents
             WHERE session = ?1 AND (name = ?2 OR nick = ?2)
             ORDER BY CASE WHEN name = ?2 THEN 0 ELSE 1 END
             LIMIT 1",
        )?
    } else {
        conn.prepare(
            "SELECT name, nick, session, pane, pane_unique_id, agent_type, socket_path, cwd, registered_at, last_seen_at
             FROM agents
             WHERE name = ?1 OR nick = ?1
             ORDER BY CASE WHEN name = ?1 THEN 0 ELSE 1 END
             LIMIT 1",
        )?
    };
    if let Some(session) = session {
        stmt.query_row(params![session, target], row_to_agent)
            .optional()
            .map_err(Into::into)
    } else {
        stmt.query_row(params![target], row_to_agent)
            .optional()
            .map_err(Into::into)
    }
}

pub fn set_pending(envelope: &Envelope) -> Result<()> {
    let conn = open()?;
    let envelope_json = serde_json::to_string(envelope)?;
    conn.execute(
        "INSERT OR REPLACE INTO pending_messages (id, recipient_name, envelope_json, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            envelope.id,
            envelope.to,
            envelope_json,
            envelope.created_at as i64
        ],
    )?;
    Ok(())
}

pub fn clear_pending(msg_id: &str) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "DELETE FROM pending_messages WHERE id = ?1",
        params![msg_id],
    )?;
    Ok(())
}

pub fn clear_pending_for_agent(name: &str) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "DELETE FROM pending_messages WHERE recipient_name = ?1",
        params![name],
    )?;
    Ok(())
}

pub fn pending_for_agent(name: &str) -> Result<Vec<Envelope>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT envelope_json
         FROM pending_messages
         WHERE recipient_name = ?1
         ORDER BY created_at ASC",
    )?;
    let mut rows = stmt.query(params![name])?;
    let mut envelopes = Vec::new();
    while let Some(row) = rows.next()? {
        let envelope_json: String = row.get(0)?;
        let envelope = serde_json::from_str::<Envelope>(&envelope_json)?;
        envelopes.push(envelope);
    }
    Ok(envelopes)
}

pub fn pending_message(msg_id: &str) -> Result<Option<Envelope>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT envelope_json
         FROM pending_messages
         WHERE id = ?1
         LIMIT 1",
    )?;
    stmt.query_row(params![msg_id], |row| row.get::<_, String>(0))
        .optional()?
        .map(|envelope_json| serde_json::from_str::<Envelope>(&envelope_json))
        .transpose()
        .map_err(Into::into)
}

pub fn set_outbound_request(
    msg_id: &str,
    sender_name: &str,
    sender_label: &str,
    recipient_name: &str,
    transport_name: &str,
    transport_target: &str,
    created_at: u64,
) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "INSERT OR REPLACE INTO outbound_requests (
            msg_id, sender_name, sender_label, recipient_name, transport_name, transport_target, created_at, nudge_count
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
        params![
            msg_id,
            sender_name,
            sender_label,
            recipient_name,
            transport_name,
            transport_target,
            created_at as i64,
        ],
    )?;
    Ok(())
}

pub fn delete_outbound_request(msg_id: &str) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "DELETE FROM outbound_requests WHERE msg_id = ?1",
        params![msg_id],
    )?;
    Ok(())
}

pub fn delete_outbound_for_sender(name: &str) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "DELETE FROM outbound_requests WHERE sender_name = ?1",
        params![name],
    )?;
    Ok(())
}

pub fn resolve_reply(msg_id: &str) -> Result<()> {
    let conn = open()?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM pending_messages WHERE id = ?1",
        params![msg_id],
    )?;
    tx.execute(
        "DELETE FROM outbound_requests WHERE msg_id = ?1",
        params![msg_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn outbound_request(msg_id: &str) -> Result<Option<OutboundRequestRecord>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT msg_id, sender_name, sender_label, recipient_name, transport_name, transport_target, created_at, nudge_count
         FROM outbound_requests
         WHERE msg_id = ?1
         LIMIT 1",
    )?;
    stmt.query_row(params![msg_id], row_to_outbound)
        .optional()
        .map_err(Into::into)
}

pub fn outbound_for_sender(name: &str) -> Result<Vec<OutboundRequestRecord>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT msg_id, sender_name, sender_label, recipient_name, transport_name, transport_target, created_at, nudge_count
         FROM outbound_requests
         WHERE sender_name = ?1
         ORDER BY created_at ASC",
    )?;
    let mut rows = stmt.query(params![name])?;
    let mut requests = Vec::new();
    while let Some(row) = rows.next()? {
        requests.push(row_to_outbound(row)?);
    }
    Ok(requests)
}

pub fn expired_outbound_for_sender(
    name: &str,
    created_at_cutoff: u64,
) -> Result<Vec<OutboundRequestRecord>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT msg_id, sender_name, sender_label, recipient_name, transport_name, transport_target, created_at, nudge_count
         FROM outbound_requests
         WHERE sender_name = ?1 AND created_at <= ?2
         ORDER BY created_at ASC",
    )?;
    let mut rows = stmt.query(params![name, created_at_cutoff as i64])?;
    let mut requests = Vec::new();
    while let Some(row) = rows.next()? {
        requests.push(row_to_outbound(row)?);
    }
    Ok(requests)
}

pub fn increment_nudge_count(msg_id: &str) -> Result<u32> {
    let conn = open()?;
    conn.execute(
        "UPDATE outbound_requests
         SET nudge_count = nudge_count + 1
         WHERE msg_id = ?1",
        params![msg_id],
    )?;
    let count = conn.query_row(
        "SELECT nudge_count FROM outbound_requests WHERE msg_id = ?1",
        params![msg_id],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(count as u32)
}

fn row_to_agent(row: &rusqlite::Row<'_>) -> rusqlite::Result<BrokerAgent> {
    Ok(BrokerAgent {
        id: AgentId {
            name: row.get(0)?,
            nick: row.get(1)?,
            session: row.get(2)?,
            pane: row.get(3)?,
            agent_type: row.get(5)?,
        },
        pane_unique_id: row.get(4)?,
        socket_path: row.get(6)?,
        cwd: row.get(7)?,
        registered_at: row.get::<_, i64>(8)? as u64,
        last_seen_at: row.get::<_, i64>(9)? as u64,
    })
}

fn row_to_outbound(row: &rusqlite::Row<'_>) -> rusqlite::Result<OutboundRequestRecord> {
    Ok(OutboundRequestRecord {
        msg_id: row.get(0)?,
        sender_name: row.get(1)?,
        sender_label: row.get(2)?,
        recipient_name: row.get(3)?,
        transport_name: row.get(4)?,
        transport_target: row.get(5)?,
        created_at: row.get::<_, i64>(6)? as u64,
        nudge_count: row.get::<_, i64>(7)? as u32,
    })
}

// ---------------------------------------------------------------------------
// Cron jobs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CronJobRecord {
    pub id: String,
    pub name: Option<String>,
    pub schedule: String,
    pub action_json: String,
    pub target: String,
    pub created_by: String,
    pub created_at: u64,
    pub last_run_at: Option<u64>,
    pub run_count: u64,
    pub error_count: u64,
    pub last_error: Option<String>,
    pub active: bool,
}

pub fn create_cron_job(
    id: &str,
    name: Option<&str>,
    schedule: &str,
    action_json: &str,
    target: &str,
    created_by: &str,
) -> Result<()> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    conn.execute(
        "INSERT INTO cron_jobs (id, name, schedule, action_json, target, created_by, created_at, active)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)",
        params![id, name, schedule, action_json, target, created_by, now],
    )?;
    Ok(())
}

pub fn list_cron_jobs(active_only: bool) -> Result<Vec<CronJobRecord>> {
    let conn = open()?;
    let sql = if active_only {
        "SELECT id, name, schedule, action_json, target, created_by, created_at,
                last_run_at, run_count, error_count, last_error, active
         FROM cron_jobs WHERE active = 1 ORDER BY created_at ASC"
    } else {
        "SELECT id, name, schedule, action_json, target, created_by, created_at,
                last_run_at, run_count, error_count, last_error, active
         FROM cron_jobs ORDER BY created_at ASC"
    };
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([])?;
    let mut jobs = Vec::new();
    while let Some(row) = rows.next()? {
        jobs.push(row_to_cron_job(row)?);
    }
    Ok(jobs)
}

pub fn get_cron_job(id: &str) -> Result<Option<CronJobRecord>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT id, name, schedule, action_json, target, created_by, created_at,
                last_run_at, run_count, error_count, last_error, active
         FROM cron_jobs WHERE id = ?1 LIMIT 1",
    )?;
    stmt.query_row(params![id], row_to_cron_job)
        .optional()
        .map_err(Into::into)
}

pub fn delete_cron_job(id: &str) -> Result<bool> {
    let conn = open()?;
    let rows = conn.execute(
        "UPDATE cron_jobs SET active = 0 WHERE id = ?1 AND active = 1",
        params![id],
    )?;
    Ok(rows > 0)
}

pub fn update_cron_job_run(id: &str, error: Option<&str>) -> Result<()> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    if let Some(err_msg) = error {
        conn.execute(
            "UPDATE cron_jobs SET last_run_at = ?2, run_count = run_count + 1,
             error_count = error_count + 1, last_error = ?3 WHERE id = ?1",
            params![id, now, err_msg],
        )?;
    } else {
        conn.execute(
            "UPDATE cron_jobs SET last_run_at = ?2, run_count = run_count + 1,
             last_error = NULL WHERE id = ?1",
            params![id, now],
        )?;
    }
    Ok(())
}

fn row_to_cron_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<CronJobRecord> {
    Ok(CronJobRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        schedule: row.get(2)?,
        action_json: row.get(3)?,
        target: row.get(4)?,
        created_by: row.get(5)?,
        created_at: row.get::<_, i64>(6)? as u64,
        last_run_at: row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
        run_count: row.get::<_, i64>(8)? as u64,
        error_count: row.get::<_, i64>(9)? as u64,
        last_error: row.get(10)?,
        active: row.get::<_, i64>(11)? != 0,
    })
}

// ---------------------------------------------------------------------------
// Bus message queue — SQLite-backed transport replacing IPC sockets
// ---------------------------------------------------------------------------

/// A message waiting in the bus queue for delivery.
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    pub id: i64,
    pub sender: String,
    pub recipient: String,
    pub body: String,
    pub created_at: u64,
}

/// Enqueue a message for delivery to `recipient`.
pub fn enqueue_message(sender: &str, recipient: &str, body: &str) -> Result<()> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    conn.execute(
        "INSERT INTO bus_queue (recipient, sender, body, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![recipient, sender, body, now],
    )?;
    Ok(())
}

/// Poll for messages addressed to `recipient`. Returns all pending messages
/// and deletes them from the queue (atomic read-and-delete).
pub fn poll_messages(recipient: &str) -> Result<Vec<QueuedMessage>> {
    let conn = open()?;
    let tx = conn.unchecked_transaction()?;

    let messages: Vec<QueuedMessage> = {
        let mut stmt = tx.prepare(
            "SELECT id, sender, recipient, body, created_at FROM bus_queue WHERE recipient = ?1 ORDER BY id"
        )?;
        stmt.query_map(params![recipient], |row| {
            Ok(QueuedMessage {
                id: row.get(0)?,
                sender: row.get(1)?,
                recipient: row.get(2)?,
                body: row.get(3)?,
                created_at: row.get::<_, i64>(4)? as u64,
            })
        })?
        .filter_map(|r| r.ok())
        .collect()
    }; // stmt dropped here, releasing borrow on tx

    if !messages.is_empty() {
        tx.execute(
            "DELETE FROM bus_queue WHERE recipient = ?1",
            params![recipient],
        )?;
    }

    tx.commit()?;
    Ok(messages)
}

/// Clean up old messages (safety net for undelivered messages from dead agents).
pub fn cleanup_old_messages(max_age_secs: u64) -> Result<usize> {
    let conn = open()?;
    let cutoff = (crate::message::epoch_secs() - max_age_secs) as i64;
    let deleted = conn.execute(
        "DELETE FROM bus_queue WHERE created_at < ?1",
        params![cutoff],
    )?;
    Ok(deleted)
}

/// TOTP secret record
#[derive(Debug, Clone)]
pub struct TotpSecret {
    pub id: i64,
    pub service: String,
    pub account: String,
    pub secret: String,
    pub algorithm: String,
    pub digits: i32,
    pub period: i32,
    pub created_at: u64,
}

/// Add a TOTP secret, scoped to current user.
pub fn totp_add(
    service: &str,
    account: &str,
    secret: &str,
    algorithm: &str,
    digits: i32,
    period: i32,
) -> Result<i64> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    let uid = current_user_id();

    let secret_to_store = if get_encryption_key().is_some() {
        match encrypt(secret) {
            Ok(enc) => enc,
            Err(e) => {
                eprintln!("Warning: encryption key available but encrypt failed: {}. Storing plaintext.", e);
                secret.to_string()
            }
        }
    } else {
        secret.to_string()
    };

    conn.execute(
        "INSERT INTO totp_secrets (user_id, service, account, secret, algorithm, digits, period, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
         ON CONFLICT(user_id, service, account) DO UPDATE SET secret = ?4, algorithm = ?5, digits = ?6, period = ?7",
        params![uid, service, account, secret_to_store, algorithm, digits, period, now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// List all TOTP secrets for current user.
pub fn totp_list() -> Result<Vec<TotpSecret>> {
    let conn = open()?;
    let uid = current_user_id();
    let mut stmt = conn.prepare(
        "SELECT id, service, account, secret, algorithm, digits, period, created_at \
         FROM totp_secrets WHERE user_id IS ?1 ORDER BY service, account",
    )?;
    let mut out = Vec::new();
    let mut rows = stmt.query(params![uid])?;
    while let Some(row) = rows.next()? {
        let secret: String = row.get(3)?;
        let decrypted = if is_encrypted(&secret) {
            decrypt(&secret).unwrap_or(secret)
        } else {
            secret
        };
        out.push(TotpSecret {
            id: row.get(0)?,
            service: row.get(1)?,
            account: row.get(2)?,
            secret: decrypted,
            algorithm: row.get(4)?,
            digits: row.get(5)?,
            period: row.get(6)?,
            created_at: row.get::<_, i64>(7)? as u64,
        });
    }
    Ok(out)
}

/// Get TOTP secret for a service+account, scoped to current user.
pub fn totp_get(service: &str, account: &str) -> Result<Option<TotpSecret>> {
    let conn = open()?;
    let uid = current_user_id();
    let mut stmt = conn.prepare(
        "SELECT id, service, account, secret, algorithm, digits, period, created_at \
         FROM totp_secrets WHERE user_id IS ?1 AND service = ?2 AND account = ?3",
    )?;
    stmt.query_row(params![uid, service, account], |row| {
        let secret: String = row.get(3)?;
        let decrypted = if is_encrypted(&secret) {
            decrypt(&secret).unwrap_or(secret)
        } else {
            secret
        };
        Ok(TotpSecret {
            id: row.get(0)?,
            service: row.get(1)?,
            account: row.get(2)?,
            secret: decrypted,
            algorithm: row.get(4)?,
            digits: row.get(5)?,
            period: row.get(6)?,
            created_at: row.get::<_, i64>(7)? as u64,
        })
    })
    .optional()
    .map_err(Into::into)
}

/// Delete a TOTP secret (by id — already scoped by user via query)
pub fn totp_delete(id: i64) -> Result<()> {
    let conn = open()?;
    let uid = current_user_id();
    conn.execute(
        "DELETE FROM totp_secrets WHERE id = ?1 AND user_id IS ?2",
        params![id, uid],
    )?;
    Ok(())
}

/// KV store record
#[derive(Debug, Clone)]
pub struct KvEntry {
    pub id: i64,
    pub scope: Option<String>,
    pub key: String,
    pub value: String,
    pub created_at: u64,
    pub updated_at: u64,
}

/// Set a KV value (per-scope or global if scope is None), scoped to current user.
pub fn kv_set(scope: Option<&str>, key: &str, value: &str) -> Result<()> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    let uid = current_user_id();

    let value_to_store = if get_encryption_key().is_some() {
        match encrypt(value) {
            Ok(enc) => enc,
            Err(e) => {
                eprintln!("Warning: encryption key available but encrypt failed: {}. Storing plaintext.", e);
                value.to_string()
            }
        }
    } else {
        value.to_string()
    };

    conn.execute(
        "INSERT INTO kv_store (user_id, scope, key, value, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(user_id, scope, key) DO UPDATE SET value = ?4, updated_at = ?6",
        params![uid, scope, key, value_to_store, now, now],
    )?;
    Ok(())
}

/// Get a KV value, scoped to current user.
pub fn kv_get(scope: Option<&str>, key: &str) -> Result<Option<KvEntry>> {
    let conn = open()?;
    let uid = current_user_id();
    let scope_val = scope.map(|s| s.to_string());

    let mut stmt = conn.prepare(
        "SELECT id, scope, key, value, created_at, updated_at FROM kv_store \
         WHERE user_id IS ?1 AND scope IS ?2 AND key = ?3",
    )?;
    stmt.query_row(params![uid, scope_val.as_deref(), key], |row| {
        let value: String = row.get(3)?;
        let decrypted = if is_encrypted(&value) {
            decrypt(&value).unwrap_or(value)
        } else {
            value
        };
        Ok(KvEntry {
            id: row.get(0)?,
            scope: row.get(1)?,
            key: row.get(2)?,
            value: decrypted,
            created_at: row.get::<_, i64>(4)? as u64,
            updated_at: row.get::<_, i64>(5)? as u64,
        })
    })
    .optional()
    .map_err(Into::into)
}

/// List KV entries for a scope (or global if None), scoped to current user.
pub fn kv_list(scope: Option<&str>) -> Result<Vec<KvEntry>> {
    let conn = open()?;
    let uid = current_user_id();
    let scope_val = scope.map(|s| s.to_string());

    let mut stmt = conn.prepare(
        "SELECT id, scope, key, value, created_at, updated_at FROM kv_store \
         WHERE user_id IS ?1 AND scope IS ?2 ORDER BY key",
    )?;
    let mut out = Vec::new();
    let mut rows = stmt.query(params![uid, scope_val.as_deref()])?;
    while let Some(row) = rows.next()? {
        let value: String = row.get(3)?;
        let decrypted = if is_encrypted(&value) {
            decrypt(&value).unwrap_or(value)
        } else {
            value
        };
        out.push(KvEntry {
            id: row.get(0)?,
            scope: row.get(1)?,
            key: row.get(2)?,
            value: decrypted,
            created_at: row.get::<_, i64>(4)? as u64,
            updated_at: row.get::<_, i64>(5)? as u64,
        });
    }
    Ok(out)
}

/// Delete a KV entry, scoped to current user.
pub fn kv_delete(scope: Option<&str>, key: &str) -> Result<()> {
    let conn = open()?;
    let uid = current_user_id();
    let scope_val = scope.map(|s| s.to_string());
    conn.execute(
        "DELETE FROM kv_store WHERE user_id IS ?1 AND scope IS ?2 AND key = ?3",
        params![uid, scope_val.as_deref(), key],
    )?;
    Ok(())
}

/// Get encryption key from server (if logged in) and store in memory
pub async fn fetch_encryption_key() -> Result<Option<Vec<u8>>> {
    let token = crate::auth::auth_token().ok_or_else(|| anyhow::anyhow!("Not logged in"))?;
    let base =
        std::env::var("SIDEKAR_API_URL").unwrap_or_else(|_| "https://sidekar.dev".to_string());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = client
        .get(format!("{}/api/v1/encryption-key", base))
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .context("Failed to fetch encryption key")?;
    if !resp.status().is_success() {
        bail!("Failed to fetch encryption key: HTTP {}", resp.status());
    }
    #[derive(serde::Deserialize)]
    struct KeyResp {
        key: String,
        user_id: Option<String>,
    }
    let body: KeyResp = resp.json().await.context("Failed to parse encryption key response")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(body.key.trim())
        .context("Invalid encryption key format")?;

    set_encryption_key(decoded.clone());

    // Store user_id for scoping KV/TOTP data
    if let Some(ref uid) = body.user_id {
        set_current_user_id(uid.clone());
        // Persist in encryption_meta so we can detect account switches
        let _ = store_meta("user_id", uid);
    }

    // One-time pass per user: encrypt any plaintext rows owned by this user
    // (or unowned rows from before user_id scoping was added).
    let migration_key = format!("encrypted:{}", body.user_id.as_deref().unwrap_or("unknown"));
    if !has_meta(&migration_key).unwrap_or(true) {
        if let Err(e) = encrypt_plaintext_rows() {
            eprintln!("Warning: failed to encrypt existing plaintext data: {e}");
        } else {
            let _ = store_meta(&migration_key, "1");
        }
    }

    Ok(Some(decoded))
}

/// Check if a key exists in encryption_meta.
fn has_meta(key: &str) -> Result<bool> {
    let conn = open()?;
    let count: i32 = conn.query_row(
        "SELECT COUNT(*) FROM encryption_meta WHERE key = ?1",
        [key],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Store a value in encryption_meta (upsert).
fn store_meta(key: &str, value: &str) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "INSERT INTO encryption_meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
        params![key, value],
    )?;
    Ok(())
}

/// Read a value from encryption_meta.
pub fn read_meta(key: &str) -> Result<Option<String>> {
    let conn = open()?;
    conn.query_row(
        "SELECT value FROM encryption_meta WHERE key = ?1",
        [key],
        |r| r.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// One-time migration per user: encrypt all plaintext KV/TOTP rows and claim
/// any unowned rows (user_id IS NULL) for the current user.
fn encrypt_plaintext_rows() -> Result<()> {
    let conn = open()?;
    let uid = current_user_id();

    // Claim unowned rows for this user (pre-user_id data from before this migration)
    conn.execute(
        "UPDATE kv_store SET user_id = ?1 WHERE user_id IS NULL",
        params![uid],
    )?;
    conn.execute(
        "UPDATE totp_secrets SET user_id = ?1 WHERE user_id IS NULL",
        params![uid],
    )?;

    // Collect plaintext KV rows for this user
    let kv_rows: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, value FROM kv_store WHERE user_id IS ?1",
        )?;
        let mut out = Vec::new();
        let mut rows = stmt.query(params![uid])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let val: String = row.get(1)?;
            if !is_encrypted(&val) {
                out.push((id, val));
            }
        }
        out
    };

    // Collect plaintext TOTP rows for this user
    let totp_rows: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, secret FROM totp_secrets WHERE user_id IS ?1",
        )?;
        let mut out = Vec::new();
        let mut rows = stmt.query(params![uid])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let val: String = row.get(1)?;
            if !is_encrypted(&val) {
                out.push((id, val));
            }
        }
        out
    };

    // Encrypt KV rows in a transaction
    let mut kv_count = 0usize;
    if !kv_rows.is_empty() {
        let tx = conn.unchecked_transaction()?;
        for (id, plaintext) in &kv_rows {
            if let Ok(encrypted) = encrypt(plaintext) {
                tx.execute(
                    "UPDATE kv_store SET value = ?1 WHERE id = ?2",
                    params![encrypted, id],
                )?;
                kv_count += 1;
            }
        }
        tx.commit()?;
    }

    // Encrypt TOTP rows in a transaction
    let mut totp_count = 0usize;
    if !totp_rows.is_empty() {
        let tx = conn.unchecked_transaction()?;
        for (id, plaintext) in &totp_rows {
            if let Ok(encrypted) = encrypt(plaintext) {
                tx.execute(
                    "UPDATE totp_secrets SET secret = ?1 WHERE id = ?2",
                    params![encrypted, id],
                )?;
                totp_count += 1;
            }
        }
        tx.commit()?;
    }

    if kv_count > 0 || totp_count > 0 {
        eprintln!(
            "sidekar: encrypted {kv_count} KV value(s) and {totp_count} TOTP secret(s) at rest"
        );
    }

    Ok(())
}

use std::sync::Mutex;

static ENCRYPTION_KEY: Mutex<Option<Vec<u8>>> = Mutex::new(None);
static CURRENT_USER_ID: Mutex<Option<String>> = Mutex::new(None);

pub fn set_encryption_key(key: Vec<u8>) {
    let mut guard = ENCRYPTION_KEY.lock().unwrap();
    *guard = Some(key);
}

pub fn clear_encryption_key() {
    let mut guard = ENCRYPTION_KEY.lock().unwrap();
    *guard = None;
}

pub fn get_encryption_key() -> Option<Vec<u8>> {
    ENCRYPTION_KEY.lock().unwrap().clone()
}

pub fn set_current_user_id(user_id: String) {
    let mut guard = CURRENT_USER_ID.lock().unwrap();
    *guard = Some(user_id);
}

pub fn clear_current_user_id() {
    let mut guard = CURRENT_USER_ID.lock().unwrap();
    *guard = None;
}

pub fn current_user_id() -> Option<String> {
    CURRENT_USER_ID.lock().unwrap().clone()
}

pub fn is_encrypted(value: &str) -> bool {
    value.starts_with("$encrypted$")
}

pub fn encrypt(plaintext: &str) -> Result<String> {
    let key = ENCRYPTION_KEY.lock().unwrap();
    let key = key.as_ref().context("No encryption key set")?;
    let cipher = Aes256Gcm::new_from_slice(key)?;

    let nonce_bytes: [u8; 12] = rand::rng().random();
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| anyhow::anyhow!("Encryption failed: {}", e))?;

    let mut combined = nonce_bytes.to_vec();
    combined.extend(ciphertext);

    Ok(format!(
        "$encrypted${}",
        base64::engine::general_purpose::STANDARD.encode(combined)
    ))
}

pub fn decrypt(encrypted: &str) -> Result<String> {
    let key = ENCRYPTION_KEY.lock().unwrap();
    let key = key.as_ref().context("No encryption key set")?;
    let cipher = Aes256Gcm::new_from_slice(key)?;

    let data = encrypted
        .strip_prefix("$encrypted$")
        .context("Invalid encrypted format")?;

    let combined = base64::engine::general_purpose::STANDARD
        .decode(data)
        .context("Invalid base64 in encrypted data")?;

    if combined.len() < 12 {
        anyhow::bail!("Encrypted data too short");
    }

    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("Decryption failed: {}", e))?;

    String::from_utf8(plaintext).map_err(|e| anyhow::anyhow!("Invalid UTF-8: {}", e))
}

/// One row from `error_events` (local SQLite log).
#[derive(Debug, Clone)]
pub struct ErrorEventRow {
    pub id: i64,
    pub created_at: i64,
    pub source: String,
    pub message: String,
    pub details: Option<String>,
}

/// Append an error row. Prefer [`try_log_error_event`] from call sites where failure must not propagate.
pub fn log_error_event(source: &str, message: &str, details: Option<&str>) -> Result<()> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    conn.execute(
        "INSERT INTO error_events (created_at, source, message, details) VALUES (?1, ?2, ?3, ?4)",
        params![now, source, message, details],
    )?;
    Ok(())
}

/// Best-effort persist. If the DB is unavailable, the event is dropped (no stderr spam).
pub fn try_log_error_event(source: &str, message: &str, details: Option<&str>) {
    let _ = log_error_event(source, message, details);
}

/// Recent errors, newest first (cap 500).
pub fn error_events_recent(limit: usize) -> Result<Vec<ErrorEventRow>> {
    let conn = open()?;
    let lim = limit.min(500).max(1) as i64;
    let mut stmt = conn.prepare(
        "SELECT id, created_at, source, message, details FROM error_events ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![lim], |row| {
        Ok(ErrorEventRow {
            id: row.get(0)?,
            created_at: row.get(1)?,
            source: row.get(2)?,
            message: row.get(3)?,
            details: row.get(4)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_test_db_path() -> PathBuf {
        let mut bytes = [0u8; 8];
        rand::rng().fill_bytes(&mut bytes);
        env::temp_dir().join(format!(
            "sidekar-broker-test-{}.sqlite3",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ))
    }

    fn with_test_db<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
        let old_home = env::var_os("HOME");
        let temp_home = env::temp_dir().join(format!(
            "sidekar-broker-home-{}",
            fresh_test_db_path()
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("tmp")
        ));
        fs::create_dir_all(&temp_home)?;
        // Safety: tests run in-process and this helper restores HOME before returning.
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
    fn persists_pending_and_outbound() -> Result<()> {
        with_test_db(|| {
            let sender = AgentId {
                name: "sender".into(),
                nick: Some("borzoi".into()),
                session: Some("sess".into()),
                pane: Some("0:0.1".into()),
                agent_type: Some("sidekar".into()),
            };
            register_agent(&sender, Some("%1"))?;

            let envelope = Envelope::new_request(sender.clone(), "receiver", "hello");
            set_pending(&envelope)?;
            set_outbound_request(
                &envelope.id,
                &sender.name,
                &sender.display_name(),
                &envelope.to,
                "broker",
                "receiver",
                envelope.created_at,
            )?;

            let pending = pending_for_agent("receiver")?;
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].id, envelope.id);

            let outbound = outbound_for_sender("sender")?;
            assert_eq!(outbound.len(), 1);
            assert_eq!(outbound[0].msg_id, envelope.id);

            resolve_reply(&envelope.id)?;

            assert!(pending_for_agent("receiver")?.is_empty());
            assert!(outbound_for_sender("sender")?.is_empty());
            Ok(())
        })
    }
}
