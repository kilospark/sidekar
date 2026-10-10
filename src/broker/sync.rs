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
pub(crate) fn device_id(conn: &Connection) -> Result<String> {
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
    /// Refused records moved past the server's version, to win on a re-push.
    pub bumped: usize,
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
    /// The version the server holds, sent with a refusal so the client can
    /// re-merge from it.
    #[serde(default)]
    current_version: Option<i64>,
}

#[derive(serde::Deserialize)]
struct PushResponse {
    results: Vec<PushResultItem>,
}

/// The most records one push request carries; matches the server's
/// `MAX_BATCH`.
const MAX_BATCH_RECORDS: usize = 500;

/// The most ciphertext one push request carries. A Vercel function's request
/// body is capped at 4.5 MB. kv and totp records are tiny, so a count limit
/// was enough until memory archives made a single record able to reach about
/// 1.4 MB. This keeps each request under the cap, with room for the JSON
/// around it.
const MAX_BATCH_BYTES: usize = 3_000_000;

/// The most ciphertext a single record may carry. A larger one can never be
/// pushed, so it is held back, and left dirty, rather than failing every batch
/// it rides in.
const MAX_RECORD_BYTES: usize = MAX_BATCH_BYTES;

/// Split `records` into push requests of at most `max_records` records and
/// `max_bytes` of ciphertext each. A record larger than `max_bytes` goes in a
/// request of its own.
fn push_batches(
    records: &[PushRecord],
    max_records: usize,
    max_bytes: usize,
) -> Vec<&[PushRecord]> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut bytes = 0;
    for (i, record) in records.iter().enumerate() {
        let size = record.ciphertext.len();
        if i > start && (i - start == max_records || bytes + size > max_bytes) {
            batches.push(&records[start..i]);
            start = i;
            bytes = 0;
        }
        bytes += size;
    }
    if start < records.len() {
        batches.push(&records[start..]);
    }
    batches
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

/// A one-time-password row as it syncs: secret decrypted, plus HOTP's counter.
struct OtpPlain {
    secret: String,
    algorithm: String,
    digits: i32,
    period: i32,
    counter: i64,
}

fn read_totp_plain(
    conn: &Connection,
    uid: &str,
    service: &str,
    account: &str,
) -> Result<Option<OtpPlain>> {
    let row = conn
        .prepare(
            "SELECT secret, algorithm, digits, period, counter FROM totp_secrets \
             WHERE user_id = ?1 AND service = ?2 AND account = ?3",
        )?
        .query_row(params![uid, service, account], |r| {
            Ok(OtpPlain {
                secret: r.get(0)?,
                algorithm: r.get(1)?,
                digits: r.get(2)?,
                period: r.get(3)?,
                counter: r.get(4)?,
            })
        })
        .optional()?;
    let Some(mut row) = row else {
        return Ok(None);
    };
    if is_encrypted(&row.secret) {
        row.secret = decrypt(&row.secret)?;
    }
    Ok(Some(row))
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
        "totp" | "hotp" => {
            let (service, account) = split_totp_record_id(record_id)?;
            let row = read_totp_plain(conn, uid, &service, &account)?
                .ok_or_else(|| anyhow!("{kind} record '{record_id}' vanished before push"))?;
            let mut payload = json!({
                "secret": row.secret,
                "algorithm": row.algorithm,
                "digits": row.digits,
                "period": row.period,
            });
            if kind == "hotp" {
                payload["counter"] = row.counter.into();
            }
            super::encryption::sync_encrypt(key, &payload.to_string())
        }
        "memory" => {
            let payload = crate::memory::sync_payload(conn, record_id)?
                .ok_or_else(|| anyhow!("memory record '{record_id}' vanished before push"))?;
            super::encryption::sync_encrypt(key, &payload)
        }
        kind if super::bus_sync::is_bus_kind(kind) => {
            let payload = super::bus_sync::sync_payload(conn, kind, record_id)?;
            super::encryption::sync_encrypt(key, &payload)
        }
        other => bail!("unknown sync kind: {other}"),
    }
}

/// The two record channels the sync endpoint serves: secrets (kv, totp, hotp,
/// memory) and the bus (agent presence and messages, `context/bus-sync.md`).
/// Each has its own server collection and its own pull watermark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncChannel {
    Secrets,
    Bus,
}

impl SyncChannel {
    pub(crate) fn of_kind(kind: &str) -> Self {
        if super::bus_sync::is_bus_kind(kind) {
            Self::Bus
        } else {
            Self::Secrets
        }
    }

    /// The endpoint, with the query that selects this channel.
    fn url(self, base: &str) -> String {
        match self {
            Self::Secrets => format!("{base}/api/v1/sync/secrets?"),
            Self::Bus => format!("{base}/api/v1/sync/secrets?channel=bus&"),
        }
    }

    fn watermark_column(self) -> &'static str {
        match self {
            Self::Secrets => "last_pull_at",
            Self::Bus => "last_bus_pull_at",
        }
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
    push_channels(uid, budget, None).await
}

/// Push only the bus channel's dirty records: a message to another machine,
/// or this machine's presence. Quick, since it skips any secrets backlog.
pub async fn push_bus(uid: &str, budget: Duration) -> Result<PushSummary> {
    push_channels(uid, budget, Some(SyncChannel::Bus)).await
}

async fn push_channels(
    uid: &str,
    budget: Duration,
    only: Option<SyncChannel>,
) -> Result<PushSummary> {
    tokio::time::timeout(budget, async {
        let first = push_dirty_inner(uid, budget, only).await?;
        if first.bumped == 0 {
            return Ok::<_, anyhow::Error>(first);
        }
        // A refused record was moved past the version the server holds. Push
        // again now, so the conflict settles in this call rather than at the
        // next retry ten minutes on.
        let second = push_dirty_inner(uid, budget, only).await?;
        Ok(PushSummary {
            pushed: first.pushed + second.pushed,
            failed: second.failed,
            bumped: first.bumped + second.bumped,
        })
    })
    .await
    .context("sync push timed out")?
}

async fn push_dirty_inner(
    uid: &str,
    budget: Duration,
    only: Option<SyncChannel>,
) -> Result<PushSummary> {
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
        if only.is_some_and(|c| c != SyncChannel::of_kind(kind)) {
            continue;
        }
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
        if ciphertext.len() > MAX_RECORD_BYTES {
            try_log_event(
                "warn",
                "sync",
                "a dirty record is too large to push",
                Some(&format!(
                    "kind={kind} record_id={record_id}: {} bytes of ciphertext, limit {MAX_RECORD_BYTES}",
                    ciphertext.len()
                )),
            );
            continue;
        }
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
    let client = crate::http_client::client_builder()
        .timeout(budget)
        .build()?;

    // Memory and secrets never share a request. A server from before memory
    // sync rejects a whole batch over one record of a kind it doesn't know,
    // and kv and totp must not wait on that: mid-release, the new binary can
    // be downloaded a few minutes before the server that accepts memory is
    // live.
    // The bus channel goes to its own collection, so it never shares a
    // request with either.
    let (bus, records): (Vec<PushRecord>, Vec<PushRecord>) = records
        .into_iter()
        .partition(|r| SyncChannel::of_kind(&r.kind) == SyncChannel::Bus);
    let (memory, secrets): (Vec<PushRecord>, Vec<PushRecord>) =
        records.into_iter().partition(|r| r.kind == "memory");
    let batches = push_batches(&secrets, MAX_BATCH_RECORDS, MAX_BATCH_BYTES)
        .into_iter()
        .chain(push_batches(&memory, MAX_BATCH_RECORDS, MAX_BATCH_BYTES))
        .map(|b| (SyncChannel::Secrets, b))
        .chain(
            push_batches(&bus, MAX_BATCH_RECORDS, MAX_BATCH_BYTES)
                .into_iter()
                .map(|b| (SyncChannel::Bus, b)),
        );

    let mut summary = PushSummary::default();
    for (channel, batch) in batches {
        let version_by_id: HashMap<(String, String), i64> = batch
            .iter()
            .map(|r| ((r.kind.clone(), r.record_id.clone()), r.version))
            .collect();
        let deleted_ids: std::collections::HashSet<(String, String)> = batch
            .iter()
            .filter(|r| r.deleted)
            .map(|r| (r.kind.clone(), r.record_id.clone()))
            .collect();

        let resp = client
            .put(channel.url(&base).trim_end_matches(['?', '&']).to_string())
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
                if !deleted_ids.contains(&(item.kind.clone(), item.record_id.clone())) {
                    super::bus_sync::pushed(&conn, &item.kind, &item.record_id)?;
                }
                summary.pushed += 1;
            } else {
                summary.failed += 1;
                // Refused: the server already holds this version or a newer
                // one, from another device. If the row is still the change we
                // just sent, move it past the server's version so the next push
                // supersedes it, and the last writer wins. Left alone, two
                // devices that reach the same version deadlock: the push is
                // refused forever, and a pull keeps the unsent local change
                // because the versions are equal.
                if let (Some(&version), Some(current)) = (
                    version_by_id.get(&(item.kind.clone(), item.record_id.clone())),
                    item.current_version,
                ) && current >= version
                {
                    summary.bumped += conn.execute(
                        "UPDATE sync_state SET version = ?5 + 1 \
                         WHERE user_id = ?1 AND kind = ?2 AND record_id = ?3 \
                           AND version = ?4 AND dirty = 1",
                        params![uid, item.kind, item.record_id, version, current],
                    )?;
                }
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
    /// Set by a server that pages (`paged=1`) when more records follow.
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    next: Option<PullCursor>,
}

/// Where the next page starts: after the record with this `updated_at`
/// (server milliseconds) and id.
#[derive(serde::Deserialize, Debug, Clone, PartialEq, Eq)]
struct PullCursor {
    since: i64,
    after_id: String,
}

/// A guard against a server that never stops paging, not a size limit: at 3 MB
/// a page it is far beyond any real account.
const MAX_PULL_PAGES: usize = 10_000;

pub(super) fn local_sync_state(
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

pub(super) fn upsert_sync_state(
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
        "totp" | "hotp" => {
            // Only a row of the kind deleted: a secret that switched kinds
            // must not be removed by a late delete of its old kind.
            let (service, account) = split_totp_record_id(record_id)?;
            conn.execute(
                "DELETE FROM totp_secrets WHERE user_id = ?1 AND service = ?2 AND account = ?3 AND kind = ?4",
                params![uid, service, account, kind],
            )?;
        }
        "memory" => crate::memory::delete_synced(conn, uid, record_id)?,
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
    kind: &str,
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
        #[serde(default)]
        counter: i64,
    }
    let parsed: TotpPayload =
        serde_json::from_str(plaintext).context("invalid totp sync payload")?;

    let now = crate::message::epoch_secs() as i64;
    let secret_to_store = encrypt(&parsed.secret)
        .context("failed to encrypt a remote totp secret under the local key")?;
    // An HOTP counter only moves forward. Taking the larger of the two means
    // a code this device already issued is never issued again because an
    // older count arrived from elsewhere.
    conn.execute(
        "INSERT INTO totp_secrets (user_id, service, account, secret, algorithm, digits, period, created_at, kind, counter) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
         ON CONFLICT(user_id, service, account) DO UPDATE SET \
             secret = ?4, algorithm = ?5, digits = ?6, period = ?7, kind = ?9, \
             counter = CASE WHEN kind = ?9 THEN MAX(counter, ?10) ELSE ?10 END",
        params![uid, service, account, secret_to_store, parsed.algorithm, parsed.digits, parsed.period, now, kind, parsed.counter],
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
    // Per-device kv keys don't sync (see `kv_store::kv_key_syncs`). One pulled
    // from a device still on an older build is left out, not adopted.
    if kind == "kv" && !super::kv_store::kv_key_syncs(record_id) {
        return Ok(false);
    }
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
                    "totp" | "hotp" => {
                        let (service, account) = split_totp_record_id(record_id)?;
                        apply_totp_remote(conn, uid, kind, &service, &account, &plaintext)?;
                    }
                    "memory" => crate::memory::apply_synced(conn, uid, record_id, &plaintext)?,
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
    pull_channel(uid, SyncChannel::Secrets, |conn, rec| {
        apply_remote_record(
            conn,
            uid,
            &rec.kind,
            &rec.record_id,
            &rec.ciphertext,
            rec.version,
            rec.deleted,
        )
    })
    .await
}

/// Pull the bus channel: other machines' agents, and messages for this one
/// (`context/bus-sync.md`).
pub async fn pull_bus(uid: &str) -> Result<PullSummary> {
    let device = device_id(&open()?)?;
    pull_channel(uid, SyncChannel::Bus, |conn, rec| {
        if !super::bus_sync::is_bus_kind(&rec.kind) {
            // A server from before bus sync answers from the secrets store.
            return Ok(false);
        }
        super::bus_sync::apply_record(
            conn,
            uid,
            &device,
            &rec.kind,
            &rec.record_id,
            &rec.ciphertext,
            rec.version,
            rec.deleted,
        )
    })
    .await
}

/// How far behind its watermark a bus pull starts (server milliseconds).
const BUS_PULL_OVERLAP_MS: i64 = 60_000;

async fn pull_channel(
    uid: &str,
    channel: SyncChannel,
    mut apply: impl FnMut(&Connection, &RemoteRecord) -> Result<bool>,
) -> Result<PullSummary> {
    let conn = open()?;
    let watermark = channel.watermark_column();
    let since: i64 = conn
        .query_row(
            &format!("SELECT {watermark} FROM sync_meta WHERE user_id = ?1"),
            params![uid],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);

    let token = crate::auth::auth_token().context("not logged in")?;
    let base = sync_api_base();
    let client = crate::http_client::client_builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    // Memory archives made a whole-history pull able to pass the 4.5 MB cap on
    // a Vercel function's response, which failed the pull for every kind. So
    // ask for pages and follow them. A server that doesn't page ignores
    // `paged` and answers in one response with no `has_more`, which ends the
    // loop after the first page.
    // The watermark is one server instance's clock, and a record's
    // `updated_at` another's, stamped before its write committed. Skew between
    // them, or a commit landing after a concurrent pull read, can leave a
    // record at or below the watermark. kv heals on its next edit; a bus
    // message is written once, so the bus re-reads an overlap. What it pulls
    // twice is harmless: messages are claimed once, agents version-guarded.
    let since = match channel {
        SyncChannel::Bus => (since - BUS_PULL_OVERLAP_MS).max(0),
        SyncChannel::Secrets => since,
    };
    let mut summary = PullSummary::default();
    let mut cursor: Option<PullCursor> = None;
    let mut server_time = since;
    let mut finished = false;
    let endpoint = channel.url(&base);
    for _ in 0..MAX_PULL_PAGES {
        let url = match &cursor {
            None => format!("{endpoint}paged=1&since={since}"),
            Some(c) => format!(
                "{endpoint}paged=1&since={}&after_id={}",
                c.since, c.after_id
            ),
        };
        let resp = client
            .get(url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .context("failed to pull sync records")?;
        if !resp.status().is_success() {
            bail!("pull failed: HTTP {}", resp.status());
        }
        let body: PullResponse = resp.json().await.context("failed to parse pull response")?;

        for rec in &body.records {
            match apply(&conn, rec) {
                Ok(true) => summary.applied += 1,
                Ok(false) => summary.skipped += 1,
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

        // The last page's server time becomes the watermark. Pages come in
        // `updated_at` order, so a record written mid-pull either lands on a
        // later page or is newer than the watermark.
        server_time = body.server_time;
        match body.next.filter(|_| body.has_more) {
            None => {
                finished = true;
                break;
            }
            Some(next) => {
                if cursor.as_ref() == Some(&next) {
                    bail!("sync pull cursor did not advance");
                }
                cursor = Some(next);
            }
        }
    }
    if !finished {
        bail!("sync pull did not finish within {MAX_PULL_PAGES} pages");
    }

    conn.execute(
        &format!(
            "INSERT INTO sync_meta (user_id, {watermark}) VALUES (?1, ?2) \
             ON CONFLICT(user_id) DO UPDATE SET {watermark} = ?2"
        ),
        params![uid, server_time],
    )?;

    Ok(summary)
}

// ---------------------------------------------------------------------------
// Bus channel rounds (context/bus-sync.md)
// ---------------------------------------------------------------------------

/// Whether bus sync runs on this machine: logged in, and not turned off with
/// `bus_sync_interval_secs = 0`. The account, when it does.
pub fn bus_sync_account() -> Option<String> {
    if crate::config::get_usize("bus_sync_interval_secs") == 0 {
        return None;
    }
    crate::auth::auth_token()?;
    crate::broker::current_user_id().filter(|u| !u.is_empty())
}

/// One round for the daemon: publish this machine's agents, pull when
/// something here can be messaged, then push what is pending, which includes
/// the tombstones of messages the pull just delivered.
pub async fn bus_sync_round(uid: &str) -> Result<()> {
    let has_agents = {
        let conn = open()?;
        let device = device_id(&conn)?;
        super::bus_sync::reconcile_presence(&conn, uid, &device)?;
        super::bus_sync::has_published_agents(&conn)?
    };
    if has_agents {
        pull_bus(uid).await?;
    }
    let pending: bool = open()?.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_state WHERE user_id = ?1 AND dirty = 1 AND kind IN ('agent', 'bus'))",
        params![uid],
        |r| r.get(0),
    )?;
    if pending {
        push_bus(uid, Duration::from_secs(20)).await?;
    }
    Ok(())
}

/// The body of `sidekar _bus_sync`: one round, the same the daemon runs every
/// `bus_sync_interval_secs`. For diagnosing bus sync without waiting on the
/// daemon, and for driving it in tests.
pub async fn run_bus_sync_once() -> Result<()> {
    let uid = bus_sync_ready()
        .await?
        .context("bus sync is off (bus_sync_interval_secs = 0) or this machine is not logged in")?;
    bus_sync_round(&uid).await
}

/// [`bus_sync_account`] for a long-lived or early caller: loads the account
/// key first, which is also what sets the account id. A daemon started before
/// login, or a command routed before the usual key fetch, has neither yet.
pub async fn bus_sync_ready() -> Result<Option<String>> {
    if crate::config::get_usize("bus_sync_interval_secs") == 0
        || crate::auth::auth_token().is_none()
    {
        return Ok(None);
    }
    super::ensure_account_key().await?;
    Ok(bus_sync_account())
}

/// Run a sync future to completion from synchronous code, which in sidekar
/// often runs inside a runtime already: on a thread of its own, with a
/// runtime of its own. `None` if that could not be set up.
pub(crate) fn run_blocking<T: Send + 'static>(
    fut: impl std::future::Future<Output = T> + Send + 'static,
) -> Option<T> {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()
            .map(|rt| rt.block_on(fut))
    })
    .join()
    .ok()
    .flatten()
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
    for key in kv_keys.iter().filter(|k| super::kv_store::kv_key_syncs(k)) {
        seed_one(conn, uid, "kv", key, now)?;
    }

    let totp_pairs: Vec<(String, String, String)> = {
        let mut stmt =
            conn.prepare("SELECT service, account, kind FROM totp_secrets WHERE user_id = ?1")?;
        let mut out = Vec::new();
        let mut rows = stmt.query(params![uid])?;
        while let Some(row) = rows.next()? {
            out.push((row.get(0)?, row.get(1)?, row.get(2)?));
        }
        out
    };
    for (service, account, kind) in totp_pairs {
        let record_id = totp_record_id(&service, &account);
        seed_one(conn, uid, super::totp::sync_kind(&kind), &record_id, now)?;
    }

    // Memory: rows written while nobody was logged in are claimed for this
    // account, then every memory the account owns is seeded.
    crate::memory::claim_unowned(conn, uid)?;
    for mem_uid in crate::memory::owned_uids(conn, uid)? {
        seed_one(conn, uid, "memory", &mem_uid, now)?;
    }

    Ok(())
}

/// How long after one background push attempt the next may start.
pub(crate) const PUSH_RETRY_SECS: i64 = 600;

/// The body of `sidekar _sync_push`: load the account key, push the dirty
/// backlog, exit. Everything is best-effort; rows that do not make it stay
/// dirty for the next attempt.
pub async fn run_sync_push_worker() -> Result<()> {
    if crate::auth::auth_token().is_none() {
        return Ok(());
    }
    // A fresh process has no key in memory, and push encrypts with it.
    if let Err(e) = super::fetch_encryption_key().await {
        try_log_event(
            "warn",
            "sync",
            "sync push worker: no account key",
            Some(&format!("{e:#}")),
        );
        return Ok(());
    }
    let uid = super::current_user_id().unwrap_or_default();
    if uid.is_empty() {
        return Ok(());
    }
    if let Err(e) = push_dirty(&uid, Duration::from_secs(60)).await {
        try_log_event(
            "warn",
            "sync",
            "background sync push failed",
            Some(&format!("{e:#}")),
        );
    }
    Ok(())
}

/// Whether rows are waiting to be pushed and no attempt ran in the last
/// [`PUSH_RETRY_SECS`]; if so, record this attempt. A failed push is retried
/// this way — from the next command after the wait — rather than only when
/// something else changes on this device.
fn claim_push_retry(uid: &str, now: i64) -> Result<bool> {
    let conn = open()?;
    let dirty: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_state WHERE user_id = ?1 AND dirty = 1)",
        params![uid],
        |r| r.get(0),
    )?;
    if !dirty {
        return Ok(false);
    }
    let last_attempt: i64 = conn
        .query_row(
            "SELECT last_push_attempt_at FROM sync_meta WHERE user_id = ?1",
            params![uid],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    if now - last_attempt < PUSH_RETRY_SECS {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO sync_meta (user_id, last_pull_at, last_push_attempt_at) VALUES (?1, 0, ?2) \
         ON CONFLICT(user_id) DO UPDATE SET last_push_attempt_at = ?2",
        params![uid, now],
    )?;
    Ok(true)
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
    if now - last_attempt < PUSH_RETRY_SECS {
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
    // Memories written since the last sync while nobody was logged in have no
    // owner yet; they upload to this account. Once claimed they push straight
    // away rather than waiting out the retry backoff.
    let claimed = claim_unowned_memories(uid).unwrap_or_else(|e| {
        try_log_event(
            "warn",
            "sync",
            "could not claim unowned memories",
            Some(&format!("{e:#}")),
        );
        0
    });
    if claimed > 0 || claim_push_retry(uid, crate::message::epoch_secs() as i64).unwrap_or(false)
    {
        crate::commands::spawn_detached_sync_push();
    }
    Ok(())
}

/// Claim every unowned memory for `uid` and mark it for upload. Returns how
/// many were claimed.
fn claim_unowned_memories(uid: &str) -> Result<usize> {
    let conn = open()?;
    let claimed = crate::memory::claim_unowned(&conn, uid)?;
    for mem_uid in &claimed {
        mark_dirty(&conn, uid, "memory", mem_uid, false)?;
    }
    Ok(claimed.len())
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
        "SELECT COUNT(*) FROM sync_state WHERE user_id = ?1 AND dirty = 1 AND kind NOT IN ('agent', 'bus')",
        params![uid],
        |r| r.get(0),
    )?;
    let tombstone_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sync_state WHERE user_id = ?1 AND deleted = 1 AND kind NOT IN ('agent', 'bus')",
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
