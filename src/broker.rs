//! SQLite-backed broker state for sidekar agent coordination.
//!
//! This module is intentionally narrow: it persists agent registrations,
//! pending inbound envelopes, and outbound request tracking so the bus can
//! provide durable state for bus coordination.

use crate::message::{AgentId, Envelope};
use crate::*;
use rusqlite::{Connection, OptionalExtension, params};

const DB_FILE: &str = "sidekar.sqlite3";

mod agent_registry;
mod agent_sessions;
mod auth_store;
mod bus_queue;
mod cron;
mod encryption;
mod event_log;
mod kv_store;
mod outbound;
mod proxy_log_store;
mod totp;

pub use agent_registry::*;
pub use agent_sessions::*;
pub use auth_store::*;
pub use bus_queue::*;
pub use cron::*;
pub use encryption::*;
pub use event_log::*;
pub use kv_store::*;
pub use outbound::*;
pub use proxy_log_store::*;
pub use totp::*;

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

pub(crate) fn open() -> Result<Connection> {
    fs::create_dir_all(data_dir())?;
    let path = db_path();
    let conn = Connection::open(&path)
        .with_context(|| format!("failed to open database at {}", path.display()))?;
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

        CREATE TABLE IF NOT EXISTS pending_requests (
            id TEXT PRIMARY KEY,
            recipient_name TEXT NOT NULL,
            envelope_json TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_pending_recipient
            ON pending_requests(recipient_name, created_at);

        CREATE TABLE IF NOT EXISTS outbound_requests (
            msg_id TEXT PRIMARY KEY,
            sender_name TEXT NOT NULL,
            sender_label TEXT NOT NULL,
            recipient_name TEXT NOT NULL,
            transport_name TEXT NOT NULL,
            transport_target TEXT NOT NULL,
            kind TEXT NOT NULL DEFAULT 'request',
            channel TEXT,
            project TEXT,
            message_preview TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'open',
            created_at INTEGER NOT NULL,
            nudge_count INTEGER NOT NULL DEFAULT 0,
            last_nudged_at INTEGER,
            answered_at INTEGER,
            timed_out_at INTEGER,
            closed_at INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_outbound_sender
            ON outbound_requests(sender_name, created_at);

        CREATE TABLE IF NOT EXISTS bus_replies (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            reply_to_msg_id TEXT NOT NULL,
            reply_msg_id TEXT NOT NULL,
            sender_name TEXT NOT NULL,
            sender_label TEXT NOT NULL,
            kind TEXT NOT NULL,
            message TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            envelope_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_bus_replies_reply_to
            ON bus_replies(reply_to_msg_id, created_at);
        CREATE INDEX IF NOT EXISTS idx_bus_replies_created
            ON bus_replies(created_at);

        CREATE TABLE IF NOT EXISTS agent_sessions (
            id TEXT PRIMARY KEY,
            agent_name TEXT NOT NULL,
            agent_type TEXT,
            display_name TEXT,
            nick TEXT,
            project TEXT NOT NULL,
            channel TEXT,
            cwd TEXT,
            started_at INTEGER NOT NULL,
            ended_at INTEGER,
            last_active_at INTEGER NOT NULL,
            request_count INTEGER NOT NULL DEFAULT 0,
            reply_count INTEGER NOT NULL DEFAULT 0,
            message_count INTEGER NOT NULL DEFAULT 0,
            last_request_msg_id TEXT,
            last_reply_msg_id TEXT,
            notes TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_agent_sessions_agent_name
            ON agent_sessions(agent_name, started_at DESC);
        CREATE INDEX IF NOT EXISTS idx_agent_sessions_project
            ON agent_sessions(project, started_at DESC);
        CREATE INDEX IF NOT EXISTS idx_agent_sessions_last_active
            ON agent_sessions(last_active_at DESC);
        CREATE INDEX IF NOT EXISTS idx_outbound_sender_status
            ON outbound_requests(sender_name, status, created_at);
        ",
    )?;

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
            active INTEGER NOT NULL DEFAULT 1,
            once INTEGER NOT NULL DEFAULT 0,
            project TEXT,
            loop_interval_secs INTEGER
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
            user_id TEXT NOT NULL DEFAULT '',
            service TEXT NOT NULL,
            account TEXT NOT NULL,
            secret TEXT NOT NULL,
            algorithm TEXT NOT NULL DEFAULT 'SHA1',
            digits INTEGER NOT NULL DEFAULT 6,
            period INTEGER NOT NULL DEFAULT 30,
            created_at INTEGER NOT NULL,
            UNIQUE(user_id, service, account)
        );
        CREATE INDEX IF NOT EXISTS idx_totp_service
            ON totp_secrets(service);
        ",
    )?;

    // KV store table (global, scoped by user_id)
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS kv_store (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id TEXT NOT NULL DEFAULT '',
            key TEXT NOT NULL,
            value TEXT NOT NULL,
            tags TEXT NOT NULL DEFAULT '[]',
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(user_id, key)
        );
        ",
    )?;

    // Add tags column if missing (migration for existing DBs)
    let has_tags: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('kv_store') WHERE name='tags'")?
        .query_row([], |r| r.get::<_, i64>(0))
        .unwrap_or(0)
        > 0;
    if !has_tags {
        conn.execute_batch("ALTER TABLE kv_store ADD COLUMN tags TEXT NOT NULL DEFAULT '[]';")?;
    }

    // KV version history
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS kv_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id TEXT NOT NULL DEFAULT '',
            key TEXT NOT NULL,
            version INTEGER NOT NULL,
            value TEXT NOT NULL,
            tags TEXT NOT NULL DEFAULT '[]',
            archived_at INTEGER NOT NULL,
            UNIQUE(user_id, key, version)
        );
        CREATE INDEX IF NOT EXISTS idx_kv_history_key
            ON kv_history(user_id, key);
        ",
    )?;

    // Encryption key marker
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS encryption_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        ",
    )?;

    // Config key-value store
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS config (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        ",
    )?;

    // Event log (durable, queryable)
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at INTEGER NOT NULL,
            level TEXT NOT NULL DEFAULT 'error',
            source TEXT NOT NULL,
            message TEXT NOT NULL,
            details TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_events_created
            ON events(created_at);
        CREATE INDEX IF NOT EXISTS idx_events_level
            ON events(level);
        ",
    )?;

    // Local memory layer
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS memory_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            project TEXT NOT NULL,
            event_type TEXT NOT NULL,
            scope TEXT NOT NULL DEFAULT 'project',
            summary TEXT NOT NULL,
            summary_norm TEXT NOT NULL,
            confidence REAL NOT NULL DEFAULT 0.8,
            tags_json TEXT NOT NULL DEFAULT '[]',
            supersedes_json TEXT NOT NULL DEFAULT '[]',
            superseded_by INTEGER,
            trigger_kind TEXT NOT NULL DEFAULT 'explicit',
            source_kind TEXT NOT NULL DEFAULT 'user',
            last_reinforced_at INTEGER,
            reinforcement_count INTEGER NOT NULL DEFAULT 0,
            summary_hash TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_memory_events_project
            ON memory_events(project, created_at);
        CREATE INDEX IF NOT EXISTS idx_memory_events_type
            ON memory_events(event_type, created_at);
        CREATE INDEX IF NOT EXISTS idx_memory_events_norm
            ON memory_events(project, event_type, scope, summary_norm);
        CREATE INDEX IF NOT EXISTS idx_memory_events_hash
            ON memory_events(summary_hash);
        CREATE INDEX IF NOT EXISTS idx_memory_events_superseded_by
            ON memory_events(superseded_by);

        ",
    )?;

    conn.execute_batch(
        "
        CREATE VIRTUAL TABLE IF NOT EXISTS memory_events_fts USING fts5(
            summary,
            content='memory_events',
            content_rowid='id',
            tokenize='porter'
        );

        CREATE TRIGGER IF NOT EXISTS memory_events_ai AFTER INSERT ON memory_events BEGIN
            INSERT INTO memory_events_fts(rowid, summary) VALUES (new.id, new.summary);
        END;

        CREATE TRIGGER IF NOT EXISTS memory_events_ad AFTER DELETE ON memory_events BEGIN
            INSERT INTO memory_events_fts(memory_events_fts, rowid, summary)
            VALUES ('delete', old.id, old.summary);
        END;

        CREATE TRIGGER IF NOT EXISTS memory_events_au AFTER UPDATE ON memory_events BEGIN
            INSERT INTO memory_events_fts(memory_events_fts, rowid, summary)
            VALUES ('delete', old.id, old.summary);
            INSERT INTO memory_events_fts(rowid, summary) VALUES (new.id, new.summary);
        END;
        ",
    )?;
    let _ = conn.execute(
        "INSERT INTO memory_events_fts(memory_events_fts) VALUES ('rebuild')",
        [],
    );

    // Local task graph
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS tasks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            notes TEXT,
            scope TEXT NOT NULL DEFAULT 'project',
            project TEXT,
            status TEXT NOT NULL DEFAULT 'open',
            priority INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            completed_at INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_tasks_status
            ON tasks(status, priority DESC, created_at DESC);

        CREATE TABLE IF NOT EXISTS task_dependencies (
            task_id INTEGER NOT NULL,
            depends_on_task_id INTEGER NOT NULL,
            created_at INTEGER NOT NULL,
            PRIMARY KEY(task_id, depends_on_task_id),
            CHECK(task_id != depends_on_task_id),
            FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE CASCADE,
            FOREIGN KEY(depends_on_task_id) REFERENCES tasks(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_task_dependencies_depends_on
            ON task_dependencies(depends_on_task_id, task_id);
        ",
    )?;
    conn.execute_batch(
        "
        CREATE INDEX IF NOT EXISTS idx_tasks_scope
            ON tasks(scope, project, status, priority DESC, created_at DESC);
        ",
    )?;

    // Proxy payload log
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS proxy_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at INTEGER NOT NULL,
            method TEXT NOT NULL,
            path TEXT NOT NULL,
            upstream_host TEXT NOT NULL,
            request_headers TEXT,
            request_body BLOB,
            response_status INTEGER,
            response_headers TEXT,
            response_body BLOB,
            duration_ms INTEGER,
            compressed INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_proxy_log_created
            ON proxy_log(created_at);
        ",
    )?;

    Ok(())
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
        let _guard = crate::test_home_lock()
            .lock()
            .map_err(|_| anyhow!("failed to lock test HOME mutex"))?;
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
                &envelope,
                &sender.display_name(),
                "broker",
                "receiver",
                sender.session.as_deref(),
                Some("/tmp/project"),
            )?;

            let pending = pending_for_agent("receiver")?;
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].id, envelope.id);

            let outbound = outbound_for_sender("sender")?;
            assert_eq!(outbound.len(), 1);
            assert_eq!(outbound[0].msg_id, envelope.id);
            assert_eq!(outbound[0].status, OUTBOUND_STATUS_OPEN);
            assert_eq!(outbound[0].kind, "request");

            let reply = Envelope::new_response(
                AgentId::new("receiver"),
                "sender",
                "done",
                envelope.id.clone(),
            );
            record_reply(&envelope.id, &reply)?;

            assert!(pending_for_agent("receiver")?.is_empty());
            assert!(outbound_for_sender("sender")?.is_empty());

            let stored = outbound_request(&envelope.id)?.context("missing outbound request")?;
            assert_eq!(stored.status, OUTBOUND_STATUS_ANSWERED);
            assert_eq!(stored.answered_at, Some(reply.created_at));

            let replies = replies_for_request(&envelope.id)?;
            assert_eq!(replies.len(), 1);
            assert_eq!(replies[0].reply_msg_id, reply.id);
            assert_eq!(replies[0].message, "done");
            Ok(())
        })
    }

    #[test]
    fn marks_outbound_timeouts_without_deleting_history() -> Result<()> {
        with_test_db(|| {
            let sender = AgentId {
                name: "sender".into(),
                nick: Some("borzoi".into()),
                session: Some("sess".into()),
                pane: Some("0:0.1".into()),
                agent_type: Some("sidekar".into()),
            };
            let envelope = Envelope::new_request(sender.clone(), "receiver", "hello");
            set_outbound_request(
                &envelope,
                &sender.display_name(),
                "broker",
                "receiver",
                sender.session.as_deref(),
                Some("/tmp/project"),
            )?;

            mark_outbound_timed_out(&envelope.id, envelope.created_at + 60)?;

            let open = outbound_for_sender("sender")?;
            assert!(open.is_empty());

            let timed_out =
                list_outbound_requests_for_sender("sender", Some(OUTBOUND_STATUS_TIMED_OUT), 10)?;
            assert_eq!(timed_out.len(), 1);
            assert_eq!(timed_out[0].msg_id, envelope.id);
            assert_eq!(timed_out[0].timed_out_at, Some(envelope.created_at + 60));
            Ok(())
        })
    }

    #[test]
    fn persists_agent_sessions_and_updates_counters() -> Result<()> {
        with_test_db(|| {
            let started_at = 1_700_000_000u64;
            create_agent_session(
                "pty:123:1700000000",
                "sender",
                Some("claude"),
                Some("borzoi"),
                "/tmp/project",
                Some("/tmp/project"),
                Some("/tmp/project"),
                started_at,
            )?;

            let sender = AgentId {
                name: "sender".into(),
                nick: Some("borzoi".into()),
                session: Some("/tmp/project".into()),
                pane: Some("0:0.1".into()),
                agent_type: Some("sidekar".into()),
            };
            let envelope = Envelope::new_request(sender.clone(), "receiver", "hello");
            set_outbound_request(
                &envelope,
                &sender.display_name(),
                "broker",
                "receiver",
                sender.session.as_deref(),
                Some("/tmp/project"),
            )?;
            mark_agent_session_request(&sender.name, &envelope.id, envelope.created_at)?;

            let reply = Envelope::new_response(
                AgentId::new("receiver"),
                "sender",
                "done",
                envelope.id.clone(),
            );
            record_reply(&envelope.id, &reply)?;
            finish_agent_session("pty:123:1700000000", reply.created_at + 10)?;

            let sessions = list_agent_sessions(false, Some("/tmp/project"), 10)?;
            assert_eq!(sessions.len(), 1);
            let session = &sessions[0];
            assert_eq!(session.agent_name, "sender");
            assert_eq!(session.agent_type.as_deref(), Some("claude"));
            assert_eq!(session.request_count, 1);
            assert_eq!(session.reply_count, 1);
            assert_eq!(session.message_count, 2);
            assert_eq!(
                session.last_request_msg_id.as_deref(),
                Some(envelope.id.as_str())
            );
            assert_eq!(
                session.last_reply_msg_id.as_deref(),
                Some(reply.id.as_str())
            );
            assert_eq!(session.ended_at, Some(reply.created_at + 10));

            let fetched = get_agent_session("pty:123:1700000000")?.context("missing session")?;
            assert_eq!(fetched.id, "pty:123:1700000000");
            Ok(())
        })
    }

    #[test]
    fn agent_session_display_name_and_notes_are_persisted() -> Result<()> {
        with_test_db(|| {
            create_agent_session(
                "pty:321:1700000000",
                "sender",
                Some("codex"),
                Some("otter"),
                "/tmp/project",
                Some("/tmp/project"),
                Some("/tmp/project"),
                1_700_000_000,
            )?;

            assert!(set_agent_session_display_name(
                "pty:321:1700000000",
                Some("Review worker")
            )?);
            assert!(set_agent_session_notes(
                "pty:321:1700000000",
                Some("Owned the PR review thread")
            )?);

            let session = get_agent_session("pty:321:1700000000")?.context("missing session")?;
            assert_eq!(session.display_name.as_deref(), Some("Review worker"));
            assert_eq!(session.notes.as_deref(), Some("Owned the PR review thread"));

            assert!(set_agent_session_notes("pty:321:1700000000", None)?);
            let cleared = get_agent_session("pty:321:1700000000")?.context("missing session")?;
            assert_eq!(cleared.notes, None);
            Ok(())
        })
    }
}
