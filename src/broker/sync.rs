//! Same-account cross-device secret sync.
//!
//! KV and TOTP rows live in local SQLite only; this module pushes local
//! mutations to a server-side ciphertext store and pulls remote mutations
//! back down, so two devices on the same account converge instead of each
//! seeing only what was ever written on that one machine.
//!
//! Server-side storage, the account-key lifecycle, and the relay
//! cross-account secret RPC (`context/cross-account-secrets.md`) are all
//! out of scope and untouched by this module.

use super::*;
use base64::Engine;
use rand::Rng;
use std::collections::HashMap;

/// `record_id` for a totp secret: the two are joined with CHAR(0) since
/// neither a service nor an account name can legally contain a NUL byte,
/// so the join is unambiguous to split back apart.
pub(crate) fn totp_record_id(service: &str, account: &str) -> String {
    format!("{service}\u{0}{account}")
}

fn split_totp_record_id(record_id: &str) -> Result<(String, String)> {
    record_id
        .split_once('\u{0}')
        .map(|(s, a)| (s.to_string(), a.to_string()))
        .ok_or_else(|| anyhow!("malformed totp sync record id"))
}

const DEVICE_ID_META_KEY: &str = "sync_device_id";

/// Opaque per-install id, persisted once. Diagnostics only (shows up in the
/// server's `device_id` column) -- never load-bearing for merge decisions.
fn device_id(conn: &Connection) -> Result<String> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM encryption_meta WHERE key = ?1",
            params![DEVICE_ID_META_KEY],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(v) = stored {
        return Ok(v);
    }
    let bytes: [u8; 16] = rand::rng().random();
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    conn.execute(
        "INSERT INTO encryption_meta (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = ?2",
        params![DEVICE_ID_META_KEY, encoded],
    )?;
    Ok(encoded)
}

// ---------------------------------------------------------------------------
// mark_dirty
// ---------------------------------------------------------------------------

/// Upsert `sync_state` for a local mutation, bumping `version` and setting
/// `dirty = 1`. Synchronous and durable before any network attempt -- this
/// is what makes the next push (whenever it happens) the retry mechanism
/// for this write, not a best-effort side channel that can lose it.
pub fn mark_dirty(
    conn: &Connection,
    uid: &str,
    kind: &str,
    record_id: &str,
    deleted: bool,
) -> Result<i64> {
    let now = crate::message::epoch_secs() as i64;
    conn.execute(
        "INSERT INTO sync_state (user_id, kind, record_id, version, deleted, dirty, updated_at) \
         VALUES (?1, ?2, ?3, 1, ?4, 1, ?5) \
         ON CONFLICT(user_id, kind, record_id) DO UPDATE SET \
             version = version + 1, deleted = ?4, dirty = 1, updated_at = ?5",
        params![uid, kind, record_id, deleted as i64, now],
    )?;
    conn.query_row(
        "SELECT version FROM sync_state WHERE user_id = ?1 AND kind = ?2 AND record_id = ?3",
        params![uid, kind, record_id],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

// ---------------------------------------------------------------------------
// Pure merge logic
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalState {
    pub version: i64,
    pub dirty: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteState {
    pub version: i64,
    pub deleted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeAction {
    /// No local row, or the remote version is strictly newer: decrypt (or
    /// delete, if the remote record is a tombstone) and adopt it locally.
    ApplyRemote,
    /// Local has an unpushed edit that is at least as new: keep it, leave
    /// `dirty` set so the next push re-asserts it.
    KeepLocal,
    /// Nothing to do.
    NoOp,
}

/// Decide what a pulled remote record should do to local state. Pure and
/// I/O-free on purpose so the merge rule can be exhaustively unit tested
/// without a database or network.
pub fn resolve(local: Option<LocalState>, remote: RemoteState) -> MergeAction {
    match local {
        None => MergeAction::ApplyRemote,
        Some(l) if remote.version > l.version => MergeAction::ApplyRemote,
        Some(l) if l.dirty => MergeAction::KeepLocal,
        Some(_) => MergeAction::NoOp,
    }
}

// ---------------------------------------------------------------------------
// Push
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PushSummary {
    pub pushed: usize,
    pub failed: usize,
}

#[derive(serde::Serialize, Clone)]
struct PushRecord {
    kind: String,
    record_id: String,
    ciphertext: String,
    version: i64,
    device_id: String,
    deleted: bool,
}

#[derive(serde::Serialize)]
struct PushBody<'a> {
    records: &'a [PushRecord],
}

#[derive(serde::Deserialize)]
struct PushResultItem {
    kind: String,
    record_id: String,
    accepted: bool,
}

#[derive(serde::Deserialize)]
struct PushResponse {
    results: Vec<PushResultItem>,
}

fn read_kv_plain(conn: &Connection, uid: &str, key: &str) -> Result<Option<(String, Vec<String>)>> {
    let row: Option<(String, String)> = conn
        .prepare("SELECT value, tags FROM kv_store WHERE user_id = ?1 AND key = ?2")?
        .query_row(params![uid, key], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((value, tags_json)) = row else {
        return Ok(None);
    };
    let decrypted = if is_encrypted(&value) {
        decrypt(&value)?
    } else {
        value
    };
    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
    Ok(Some((decrypted, tags)))
}

fn read_totp_plain(
    conn: &Connection,
    uid: &str,
    service: &str,
    account: &str,
) -> Result<Option<(String, String, i32, i32)>> {
    let row: Option<(String, String, i32, i32)> = conn
        .prepare(
            "SELECT secret, algorithm, digits, period FROM totp_secrets \
             WHERE user_id = ?1 AND service = ?2 AND account = ?3",
        )?
        .query_row(params![uid, service, account], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    let Some((secret, algorithm, digits, period)) = row else {
        return Ok(None);
    };
    let decrypted = if is_encrypted(&secret) {
        decrypt(&secret)?
    } else {
        secret
    };
    Ok(Some((decrypted, algorithm, digits, period)))
}

fn build_ciphertext(
    conn: &Connection,
    uid: &str,
    kind: &str,
    record_id: &str,
    key: &[u8],
) -> Result<String> {
    match kind {
        "kv" => {
            let (value, tags) = read_kv_plain(conn, uid, record_id)?
                .ok_or_else(|| anyhow!("kv record '{record_id}' vanished before push"))?;
            let payload = json!({ "value": value, "tags": tags }).to_string();
            super::encryption::sync_encrypt(key, &payload)
        }
        "totp" => {
            let (service, account) = split_totp_record_id(record_id)?;
            let (secret, algorithm, digits, period) =
                read_totp_plain(conn, uid, &service, &account)?
                    .ok_or_else(|| anyhow!("totp record '{record_id}' vanished before push"))?;
            let payload = json!({
                "secret": secret,
                "algorithm": algorithm,
                "digits": digits,
                "period": period,
            })
            .to_string();
            super::encryption::sync_encrypt(key, &payload)
        }
        other => bail!("unknown sync kind: {other}"),
    }
}

fn sync_api_base() -> String {
    std::env::var("SIDEKAR_API_URL").unwrap_or_else(|_| "https://sidekar.dev".to_string())
}

/// Push every dirty row for `uid` to the server. Every mutating command
/// calls this, and it always reads the *whole* dirty backlog rather than
/// just the key that was just touched -- that's the retry mechanism for a
/// push that failed earlier while offline.
pub async fn push_dirty(uid: &str, budget: Duration) -> Result<PushSummary> {
    tokio::time::timeout(budget, push_dirty_inner(uid, budget))
        .await
        .context("sync push timed out")?
}

async fn push_dirty_inner(uid: &str, budget: Duration) -> Result<PushSummary> {
    let conn = open()?;
    let key = get_encryption_key().context("no active encryption key to push sync records")?;
    let dev_id = device_id(&conn)?;

    let dirty: Vec<(String, String, i64, bool)> = {
        let mut stmt = conn.prepare(
            "SELECT kind, record_id, version, deleted FROM sync_state \
             WHERE user_id = ?1 AND dirty = 1",
        )?;
        let mut out = Vec::new();
        let mut rows = stmt.query(params![uid])?;
        while let Some(row) = rows.next()? {
            out.push((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, i64>(3)? != 0,
            ));
        }
        out
    };

    if dirty.is_empty() {
        return Ok(PushSummary::default());
    }

    let mut records = Vec::with_capacity(dirty.len());
    for (kind, record_id, version, deleted) in &dirty {
        let ciphertext = if *deleted {
            String::new()
        } else {
            match build_ciphertext(&conn, uid, kind, record_id, &key) {
                Ok(c) => c,
                Err(e) => {
                    try_log_event(
                        "warn",
                        "sync",
                        "failed to prepare a dirty record for push",
                        Some(&format!("kind={kind} record_id={record_id}: {e:#}")),
                    );
                    continue;
                }
            }
        };
        records.push(PushRecord {
            kind: kind.clone(),
            record_id: record_id.clone(),
            ciphertext,
            version: *version,
            device_id: dev_id.clone(),
            deleted: *deleted,
        });
    }

    if records.is_empty() {
        return Ok(PushSummary::default());
    }

    let token = crate::auth::auth_token().context("not logged in")?;
    let base = sync_api_base();
    let client = reqwest::Client::builder().timeout(budget).build()?;

    let mut summary = PushSummary::default();
    for batch in records.chunks(500) {
        let version_by_id: HashMap<(String, String), i64> = batch
            .iter()
            .map(|r| ((r.kind.clone(), r.record_id.clone()), r.version))
            .collect();

        let resp = client
            .put(format!("{base}/api/v1/sync/secrets"))
            .header("Authorization", format!("Bearer {token}"))
            .json(&PushBody { records: batch })
            .send()
            .await
            .context("failed to push sync records")?;

        if !resp.status().is_success() {
            summary.failed += batch.len();
            continue;
        }

        let parsed: PushResponse = resp.json().await.context("failed to parse push response")?;
        for item in parsed.results {
            if item.accepted {
                if let Some(&version) =
                    version_by_id.get(&(item.kind.clone(), item.record_id.clone()))
                {
                    conn.execute(
                        "UPDATE sync_state SET dirty = 0 \
                         WHERE user_id = ?1 AND kind = ?2 AND record_id = ?3 AND version = ?4",
                        params![uid, item.kind, item.record_id, version],
                    )?;
                }
                summary.pushed += 1;
            } else {
                summary.failed += 1;
            }
        }
    }

    Ok(summary)
}

// ---------------------------------------------------------------------------
// Pull + merge
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PullSummary {
    pub applied: usize,
    pub skipped: usize,
}

#[derive(serde::Deserialize)]
struct RemoteRecord {
    kind: String,
    record_id: String,
    ciphertext: String,
    version: i64,
    #[serde(default)]
    deleted: bool,
}

#[derive(serde::Deserialize)]
struct PullResponse {
    records: Vec<RemoteRecord>,
    server_time: i64,
}

fn local_sync_state(
    conn: &Connection,
    uid: &str,
    kind: &str,
    record_id: &str,
) -> Result<Option<LocalState>> {
    conn.prepare(
        "SELECT version, dirty FROM sync_state WHERE user_id = ?1 AND kind = ?2 AND record_id = ?3",
    )?
    .query_row(params![uid, kind, record_id], |r| {
        Ok(LocalState {
            version: r.get(0)?,
            dirty: r.get::<_, i64>(1)? != 0,
        })
    })
    .optional()
    .map_err(Into::into)
}

fn upsert_sync_state(
    conn: &Connection,
    uid: &str,
    kind: &str,
    record_id: &str,
    version: i64,
    deleted: bool,
    dirty: bool,
) -> Result<()> {
    let now = crate::message::epoch_secs() as i64;
    conn.execute(
        "INSERT INTO sync_state (user_id, kind, record_id, version, deleted, dirty, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(user_id, kind, record_id) DO UPDATE SET \
             version = ?4, deleted = ?5, dirty = ?6, updated_at = ?7",
        params![
            uid,
            kind,
            record_id,
            version,
            deleted as i64,
            dirty as i64,
            now
        ],
    )?;
    Ok(())
}

fn delete_local_record(conn: &Connection, uid: &str, kind: &str, record_id: &str) -> Result<()> {
    match kind {
        "kv" => {
            conn.execute(
                "DELETE FROM kv_store WHERE user_id = ?1 AND key = ?2",
                params![uid, record_id],
            )?;
            conn.execute(
                "DELETE FROM kv_history WHERE user_id = ?1 AND key = ?2",
                params![uid, record_id],
            )?;
        }
        "totp" => {
            let (service, account) = split_totp_record_id(record_id)?;
            conn.execute(
                "DELETE FROM totp_secrets WHERE user_id = ?1 AND service = ?2 AND account = ?3",
                params![uid, service, account],
            )?;
        }
        other => bail!("unknown sync kind: {other}"),
    }
    Ok(())
}

/// Apply a decrypted remote kv payload locally. Archives the previous value
/// into `kv_history` first, same as `kv_set` does, so a remote-driven
/// overwrite stays locally reversible.
fn apply_kv_remote(conn: &Connection, uid: &str, key: &str, plaintext: &str) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct KvPayload {
        value: String,
        tags: Vec<String>,
    }
    let parsed: KvPayload = serde_json::from_str(plaintext).context("invalid kv sync payload")?;

    super::kv_store::kv_archive(conn, uid, key)?;

    let now = crate::message::epoch_secs() as i64;
    let tags_json = serde_json::to_string(&parsed.tags).unwrap_or_else(|_| "[]".to_string());
    let value_to_store = encrypt(&parsed.value)
        .context("failed to encrypt a remote kv value under the local key")?;
    conn.execute(
        "INSERT INTO kv_store (user_id, key, value, tags, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?5) \
         ON CONFLICT(user_id, key) DO UPDATE SET value = ?3, tags = ?4, updated_at = ?5",
        params![uid, key, value_to_store, tags_json, now],
    )?;
    Ok(())
}

/// Apply a decrypted remote totp payload locally. No history table for totp,
/// matching local `totp_add` behavior.
fn apply_totp_remote(
    conn: &Connection,
    uid: &str,
    service: &str,
    account: &str,
    plaintext: &str,
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct TotpPayload {
        secret: String,
        algorithm: String,
        digits: i32,
        period: i32,
    }
    let parsed: TotpPayload =
        serde_json::from_str(plaintext).context("invalid totp sync payload")?;

    let now = crate::message::epoch_secs() as i64;
    let secret_to_store = encrypt(&parsed.secret)
        .context("failed to encrypt a remote totp secret under the local key")?;
    conn.execute(
        "INSERT INTO totp_secrets (user_id, service, account, secret, algorithm, digits, period, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
         ON CONFLICT(user_id, service, account) DO UPDATE SET \
             secret = ?4, algorithm = ?5, digits = ?6, period = ?7",
        params![uid, service, account, secret_to_store, parsed.algorithm, parsed.digits, parsed.period, now],
    )?;
    Ok(())
}

fn apply_remote_record(
    conn: &Connection,
    uid: &str,
    kind: &str,
    record_id: &str,
    ciphertext: &str,
    remote_version: i64,
    remote_deleted: bool,
) -> Result<bool> {
    let local = local_sync_state(conn, uid, kind, record_id)?;
    let remote = RemoteState {
        version: remote_version,
        deleted: remote_deleted,
    };

    match resolve(local, remote) {
        MergeAction::ApplyRemote => {
            if remote_deleted {
                delete_local_record(conn, uid, kind, record_id)?;
            } else {
                let key = get_encryption_key()
                    .context("no active encryption key to decrypt a pulled sync record")?;
                let plaintext = super::encryption::sync_decrypt(&key, ciphertext)?;
                match kind {
                    "kv" => apply_kv_remote(conn, uid, record_id, &plaintext)?,
                    "totp" => {
                        let (service, account) = split_totp_record_id(record_id)?;
                        apply_totp_remote(conn, uid, &service, &account, &plaintext)?;
                    }
                    other => bail!("unknown sync kind: {other}"),
                }
            }
            upsert_sync_state(
                conn,
                uid,
                kind,
                record_id,
                remote_version,
                remote_deleted,
                false,
            )?;
            Ok(true)
        }
        MergeAction::KeepLocal => Ok(false),
        MergeAction::NoOp => {
            if let Some(l) = local
                && !l.dirty
                && remote_version < l.version
            {
                try_log_event(
                    "warn",
                    "sync",
                    "pulled a remote sync record older than clean local state",
                    Some(&format!("kind={kind} record_id={record_id}")),
                );
            }
            Ok(false)
        }
    }
}

/// Pull everything the server has recorded since the last watermark and
/// merge it in. The watermark is the server's response `server_time`, not
/// the max row `updated_at` in the batch, so a row written mid-response
/// isn't skipped on the next pull.
pub async fn pull_merge(uid: &str) -> Result<PullSummary> {
    let conn = open()?;
    let since: i64 = conn
        .query_row(
            "SELECT last_pull_at FROM sync_meta WHERE user_id = ?1",
            params![uid],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);

    let token = crate::auth::auth_token().context("not logged in")?;
    let base = sync_api_base();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let resp = client
        .get(format!("{base}/api/v1/sync/secrets?since={since}"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .context("failed to pull sync records")?;
    if !resp.status().is_success() {
        bail!("pull failed: HTTP {}", resp.status());
    }
    let body: PullResponse = resp.json().await.context("failed to parse pull response")?;

    let mut summary = PullSummary::default();
    for rec in &body.records {
        match apply_remote_record(
            &conn,
            uid,
            &rec.kind,
            &rec.record_id,
            &rec.ciphertext,
            rec.version,
            rec.deleted,
        ) {
            Ok(applied) => {
                if applied {
                    summary.applied += 1;
                } else {
                    summary.skipped += 1;
                }
            }
            Err(e) => {
                try_log_event(
                    "warn",
                    "sync",
                    "skipping undecryptable sync record",
                    Some(&format!(
                        "kind={} record_id={}: {:#}",
                        rec.kind, rec.record_id, e
                    )),
                );
                summary.skipped += 1;
            }
        }
    }

    conn.execute(
        "INSERT INTO sync_meta (user_id, last_pull_at) VALUES (?1, ?2) \
         ON CONFLICT(user_id) DO UPDATE SET last_pull_at = ?2",
        params![uid, body.server_time],
    )?;

    Ok(summary)
}

// ---------------------------------------------------------------------------
// Initial upload + bootstrap
// ---------------------------------------------------------------------------

fn seed_one(conn: &Connection, uid: &str, kind: &str, record_id: &str, now: i64) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_state WHERE user_id = ?1 AND kind = ?2 AND record_id = ?3)",
        params![uid, kind, record_id],
        |r| r.get(0),
    )?;
    if exists {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO sync_state (user_id, kind, record_id, version, deleted, dirty, updated_at) \
         VALUES (?1, ?2, ?3, 1, 0, 1, ?4)",
        params![uid, kind, record_id, now],
    )?;
    Ok(())
}

fn seed_sync_state(conn: &Connection, uid: &str) -> Result<()> {
    let now = crate::message::epoch_secs() as i64;

    let kv_keys: Vec<String> = {
        let mut stmt = conn.prepare("SELECT key FROM kv_store WHERE user_id = ?1")?;
        let mut out = Vec::new();
        let mut rows = stmt.query(params![uid])?;
        while let Some(row) = rows.next()? {
            out.push(row.get::<_, String>(0)?);
        }
        out
    };
    for key in kv_keys {
        seed_one(conn, uid, "kv", &key, now)?;
    }

    let totp_pairs: Vec<(String, String)> = {
        let mut stmt =
            conn.prepare("SELECT service, account FROM totp_secrets WHERE user_id = ?1")?;
        let mut out = Vec::new();
        let mut rows = stmt.query(params![uid])?;
        while let Some(row) = rows.next()? {
            out.push((row.get(0)?, row.get(1)?));
        }
        out
    };
    for (service, account) in totp_pairs {
        let record_id = totp_record_id(&service, &account);
        seed_one(conn, uid, "totp", &record_id, now)?;
    }

    Ok(())
}

/// Seed `sync_state` for the initial upload and spawn a detached background
/// worker to push it. Run once per account, before the first pull, so a device
/// upgrading from a pre-sync build uploads what it already has instead of an
/// empty pull clobbering it.
///
/// The push runs in the background so interactive commands never block on it.
/// If the worker fails, the attempt time is recorded and subsequent bootstraps
/// back off for 10 minutes.
pub fn seed_initial_upload(uid: &str) -> Result<()> {
    let conn = open()?;
    let has_meta: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_meta WHERE user_id = ?1 AND last_pull_at > 0)",
        params![uid],
        |r| r.get(0),
    )?;
    if has_meta {
        return Ok(());
    }
    let last_attempt: i64 = conn
        .query_row(
            "SELECT last_push_attempt_at FROM sync_meta WHERE user_id = ?1",
            params![uid],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let now = crate::message::epoch_secs() as i64;
    // Backoff: don't spawn a background upload more than once per 10 min.
    if now - last_attempt < 600 {
        return Ok(());
    }
    seed_sync_state(&conn, uid)?;
    drop(conn);
    // Record the attempt before spawning, so a crash doesn't cause a tight loop.
    let conn = open()?;
    conn.execute(
        "INSERT INTO sync_meta (user_id, last_pull_at, last_push_attempt_at) \
         VALUES (?1, 0, ?2) \
         ON CONFLICT(user_id) DO UPDATE SET last_push_attempt_at = ?2",
        params![uid, now],
    )?;
    drop(conn);
    crate::commands::spawn_detached_sync_push();
    Ok(())
}

/// Run once per command, right after the account encryption key loads:
/// upload anything pre-existing on a first-ever sync for this account, then
/// pull and merge whatever the server has. Callers swallow errors -- this is
/// always best-effort, same as the encryption-key fetch it follows.
///
/// The initial upload runs in a detached background worker so interactive
/// commands never block on it. The pull stays synchronous (10s max) so the
/// command sees secrets from other devices.
pub async fn sync_bootstrap(uid: &str) -> Result<()> {
    let conn = open()?;
    let has_meta: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_meta WHERE user_id = ?1 AND last_pull_at > 0)",
        params![uid],
        |r| r.get(0),
    )?;
    drop(conn);
    if !has_meta {
        // Best-effort: seed and background the upload. Failures are logged
        // and retried with backoff; the pull below still runs.
        if let Err(e) = seed_initial_upload(uid) {
            try_log_event(
                "warn",
                "sync",
                "initial sync upload setup failed",
                Some(&format!("{e:#}")),
            );
        }
    }
    pull_merge(uid).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Diagnostics (`sidekar kv sync-status`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
pub struct SyncStatus {
    pub last_pull_at: i64,
    pub dirty_count: i64,
    pub tombstone_count: i64,
}

pub fn sync_status(uid: &str) -> Result<SyncStatus> {
    let conn = open()?;
    let last_pull_at: i64 = conn
        .query_row(
            "SELECT last_pull_at FROM sync_meta WHERE user_id = ?1",
            params![uid],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let dirty_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sync_state WHERE user_id = ?1 AND dirty = 1",
        params![uid],
        |r| r.get(0),
    )?;
    let tombstone_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sync_state WHERE user_id = ?1 AND deleted = 1",
        params![uid],
        |r| r.get(0),
    )?;
    Ok(SyncStatus {
        last_pull_at,
        dirty_count,
        tombstone_count,
    })
}

#[cfg(test)]
mod tests;
