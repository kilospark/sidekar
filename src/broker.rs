//! SQLite-backed broker state for sidekar agent coordination.
//!
//! This module is intentionally narrow: it persists agent registrations,
//! pending inbound envelopes, and outbound request tracking so the bus can
//! provide durable state for bus coordination.

use crate::message::{AgentId, Envelope};
use crate::*;
use rusqlite::{Connection, OptionalExtension, params};
use std::cell::RefCell;

const DB_FILE: &str = "sidekar.sqlite3";

/// Bump when migrations are added to `init_schema`. `ensure_schema` skips
/// `init_schema` when the file's `PRAGMA user_version` already matches —
/// without that gate, every `broker::open()` (called from the input loop
/// ~10/sec while idle, more under paste burst) would re-execute every
/// `CREATE … IF NOT EXISTS` and the FTS rebuild, turning keystrokes into
/// multi-millisecond stalls that scale with the schema and the
/// `memory_events` row count.
const SCHEMA_VERSION: u32 = 4;

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

/// Truncate the WAL file so its on-disk size doesn't grow without bound on
/// long-lived daemons. Safe to call while other connections are open.
pub fn wal_checkpoint_truncate() -> Result<()> {
    let conn = open()?;
    conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
    Ok(())
}

/// Reclaim freed pages from the SQLite file. Rewrites the whole DB, so only
/// run occasionally (daily at most). Requires no other writers to be holding
/// long transactions; otherwise falls back silently.
pub fn vacuum_db() -> Result<()> {
    let conn = open()?;
    conn.execute_batch("VACUUM")?;
    Ok(())
}

pub(crate) fn open() -> Result<Connection> {
    let conn = open_raw()?;
    ensure_schema(&conn)?;
    Ok(conn)
}

/// Open a fresh connection without running schema init. Used by `ensure_schema`
/// itself (to avoid recursion) and by code that knows the schema is already up.
fn open_raw() -> Result<Connection> {
    fs::create_dir_all(data_dir())?;
    let path = db_path();
    let conn = Connection::open(&path)
        .with_context(|| format!("failed to open database at {}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    // `journal_mode=WAL` and `foreign_keys=ON` are inexpensive and required
    // per-connection (foreign_keys especially — it's not file-persistent).
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

fn ensure_schema(conn: &Connection) -> Result<()> {
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
        .unwrap_or(0);
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    init_schema(conn)?;
    // v4: drop legacy agents.socket_path (bus delivery uses broker SQLite `bus_queue`).
    if version < 4 {
        migrate_agents_drop_socket_path(conn)?;
    }
    conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
    Ok(())
}

fn migrate_agents_drop_socket_path(conn: &Connection) -> Result<()> {
    let has_socket_path = {
        let mut stmt = conn.prepare("PRAGMA table_info(agents)")?;
        stmt.query_map([], |r| r.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .any(|name| name == "socket_path")
    };
    if has_socket_path {
        conn.execute_batch("ALTER TABLE agents DROP COLUMN socket_path")?;
    }
    Ok(())
}

/// Force schema initialization at startup so the first hot-path call doesn't
/// pay for it. Safe to call multiple times — subsequent calls are no-ops.
pub fn init_db() -> Result<()> {
    let conn = open_raw()?;
    ensure_schema(&conn)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Cached connection for hot paths (e.g. the REPL bus poller)
// ---------------------------------------------------------------------------

thread_local! {
    static CACHED_CONN: RefCell<Option<Connection>> = const { RefCell::new(None) };
}

/// Run `f` with a thread-local cached `Connection`, opening one on first use.
/// SQLite handles aren't `Sync` but they're cheap to keep open per-thread,
/// and reuse avoids per-call `open()` syscalls + WAL pragma roundtrips on
/// the keystroke-frequency polling path.
pub(crate) fn with_cached_conn<F, R>(f: F) -> Result<R>
where
    F: FnOnce(&Connection) -> rusqlite::Result<R>,
{
    CACHED_CONN.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(open()?);
        }
        let conn = slot.as_ref().expect("just initialized");
        Ok(f(conn)?)
    })
}

/// Explicit rebuild of the memory FTS index. The previous design ran this on
/// every `open()`, which is O(rows) and writes to the WAL — a hidden cost on
/// the hot polling path. Call this from housekeeping or after bulk writes.
pub fn rebuild_memory_fts() -> Result<()> {
    let conn = open()?;
    conn.execute(
        "INSERT INTO memory_events_fts(memory_events_fts) VALUES ('rebuild')",
        [],
    )?;
    Ok(())
}

/// Reclaim freed pages opportunistically when the freelist exceeds `ratio`.
/// Returns true if VACUUM ran. Safe to call from a background thread.
pub fn maybe_vacuum(ratio: f64) -> Result<bool> {
    let conn = open()?;
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let freelist: i64 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
    if page_count == 0 {
        return Ok(false);
    }
    let bloat = freelist as f64 / page_count as f64;
    if bloat < ratio {
        return Ok(false);
    }
    conn.execute_batch("VACUUM")?;
    Ok(true)
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

    // REPL session persistence (previously created lazily by session.rs)
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS repl_sessions (
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
            ON repl_entries(session_id, created_at);
        CREATE TABLE IF NOT EXISTS repl_input_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            scope_root TEXT NOT NULL,
            scope_name TEXT NOT NULL DEFAULT '',
            line TEXT NOT NULL,
            created_at REAL NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_repl_input_history_scope
            ON repl_input_history(scope_root, id);

        /*
         * session_journals
         * ----------------
         * One structured summary per journaling pass during a REPL
         * session. Populated by the background journaler (see
         * src/repl/journal.rs) after N idle seconds post-Done; read
         * on session resume (system prompt injection), in /session
         * listings (teaser), and by `sidekar journal` CLI.
         *
         * Design notes:
         *   - `from_entry_id` / `to_entry_id` are TEXT because
         *     repl_entries.id is a UUID string, not an autoincrement
         *     integer. Next pass resumes from the entry strictly
         *     after to_entry_id.
         *   - `structured_json` is the full 12-section hermes-style
         *     summary; `headline` is a one-liner extracted for fast
         *     render without re-parsing the JSON.
         *   - `previous_id` gives an iterative-update chain: the
         *     second journal for a session references the first so
         *     we can walk backwards or re-compose.
         *   - `project` denormalizes repl_sessions.cwd so queries
         *     like last-journal-across-all-sessions-in-this-project
         *     don't need a join.
         *   - Audit columns (model_used, cred_used, tokens_in/out)
         *     are mandatory — cost tracking for the journaler is a
         *     hard requirement, not a nice-to-have.
         */
        CREATE TABLE IF NOT EXISTS session_journals (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES repl_sessions(id),
            project TEXT NOT NULL,
            created_at REAL NOT NULL,
            from_entry_id TEXT NOT NULL,
            to_entry_id TEXT NOT NULL,
            structured_json TEXT NOT NULL,
            headline TEXT NOT NULL,
            previous_id INTEGER REFERENCES session_journals(id),
            model_used TEXT NOT NULL,
            cred_used TEXT NOT NULL,
            tokens_in INTEGER NOT NULL DEFAULT 0,
            tokens_out INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_session_journals_session_time
            ON session_journals(session_id, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_session_journals_project_time
            ON session_journals(project, created_at DESC);

        /*
         * memory_journal_support
         * ----------------------
         * Links auto-promoted entries in `memory_events` back to the
         * journals that supported their promotion. Lets the promoter
         * answer has-this-memory-been-reinforced-lately and the
         * age-out sweep answer should-this-memory-decay with
         * deterministic queries instead of text heuristics.
         *
         * Composite PK prevents double-linking the same journal to
         * the same memory if the promoter runs twice on the same
         * content (e.g. idempotent retries after a crash).
         */
        CREATE TABLE IF NOT EXISTS memory_journal_support (
            memory_id INTEGER NOT NULL REFERENCES memory_events(id),
            journal_id INTEGER NOT NULL REFERENCES session_journals(id),
            created_at REAL NOT NULL,
            PRIMARY KEY (memory_id, journal_id)
        );
        CREATE INDEX IF NOT EXISTS idx_memory_journal_support_memory
            ON memory_journal_support(memory_id);
        CREATE INDEX IF NOT EXISTS idx_memory_journal_support_journal
            ON memory_journal_support(journal_id);

        /*
         * memory_candidates
         * -----------------
         * Auto-extracted learning candidates from journals before or alongside
         * durable promotion into memory_events. Gives the self-learning loop a
         * reviewable, queryable staging area instead of silently jumping from
         * journal text to durable memory.
         */
        CREATE TABLE IF NOT EXISTS memory_candidates (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            project TEXT NOT NULL,
            session_id TEXT NOT NULL,
            journal_id INTEGER NOT NULL REFERENCES session_journals(id),
            event_type TEXT NOT NULL,
            scope TEXT NOT NULL,
            summary TEXT NOT NULL,
            summary_norm TEXT NOT NULL,
            confidence REAL NOT NULL DEFAULT 0.0,
            status TEXT NOT NULL DEFAULT 'new',
            source_kind TEXT NOT NULL,
            trigger_kind TEXT NOT NULL,
            related_memory_id INTEGER REFERENCES memory_events(id),
            support_count INTEGER NOT NULL DEFAULT 1,
            created_at REAL NOT NULL,
            updated_at REAL NOT NULL,
            UNIQUE(project, event_type, scope, summary_norm)
        );
        CREATE INDEX IF NOT EXISTS idx_memory_candidates_project_time
            ON memory_candidates(project, updated_at DESC);
        CREATE INDEX IF NOT EXISTS idx_memory_candidates_status
            ON memory_candidates(status, updated_at DESC);

        /*
         * memory_events_usage
         * -------------------
         * Audit trail for automatic memory selection, reinforcement,
         * contradiction, and resolution. Keeps learning lifecycle
         * inspectable instead of collapsing everything into confidence
         * deltas with no provenance.
         */
        CREATE TABLE IF NOT EXISTS memory_events_usage (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            memory_id INTEGER NOT NULL REFERENCES memory_events(id),
            session_id TEXT REFERENCES repl_sessions(id),
            journal_id INTEGER REFERENCES session_journals(id),
            entry_id TEXT,
            usage_kind TEXT NOT NULL,
            detail_json TEXT NOT NULL DEFAULT '{}',
            created_at REAL NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_memory_events_usage_memory_time
            ON memory_events_usage(memory_id, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_memory_events_usage_kind_time
            ON memory_events_usage(usage_kind, created_at DESC);
        ",
    )?;

    // Memory import log — tracks files seen by `sidekar memory import` so
    // subsequent runs can skip unchanged files. Identity is
    // (source_kind, file_path). `content_hash` is a sha256 of the file
    // bytes (or normalized content for SQLite sources); if it matches
    // the last-seen value, the importer short-circuits. `batch_id`
    // groups a single invocation so the user can later audit or undo.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS memory_import_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_kind TEXT NOT NULL,
            file_path TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            batch_id TEXT NOT NULL,
            events_created INTEGER NOT NULL DEFAULT 0,
            events_deduped INTEGER NOT NULL DEFAULT 0,
            imported_at INTEGER NOT NULL,
            UNIQUE(source_kind, file_path)
        );
        CREATE INDEX IF NOT EXISTS idx_memory_import_log_batch
            ON memory_import_log(batch_id);
        CREATE INDEX IF NOT EXISTS idx_memory_import_log_imported
            ON memory_import_log(imported_at);
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
mod tests;
