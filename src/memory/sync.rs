//! Cross-device sync for memory (#31): the memory side of `broker::sync`.
//!
//! Memory rides the same encrypted record sync as kv/totp/hotp. Each row is a
//! record of kind `memory`, named by its `uid`, encrypted for the server, and
//! merged by version. It differs from the secret stores in three ways:
//!
//! - **Ownership.** kv and totp rows are scoped to the account that wrote them.
//!   Memory never was: everything on the machine is readable whichever account
//!   is logged in, and that stays true. What is per-account is the upload.
//!   `sync_owner` records the account a row syncs to: the one logged in when
//!   it was written or claimed, or the one it was pulled from. So logging into
//!   a second account never copies these memories into it.
//! - **Content only.** Supersede links (`supersedes_json`, `superseded_by`) name
//!   rows by local autoincrement id, which means nothing on another device, so
//!   they stay local. Reinforcement is a local relevance signal: it travels
//!   with a row the first time the row arrives, but search hits don't mark a
//!   row changed, or every search would upload.
//! - **No echo.** Local changes call [`touch`]; applying a pulled record does
//!   not, so a change from another device is never pushed back as a new
//!   version.

use super::*;

/// A sync uid for a new memory row: 128 random bits as hex, the same shape the
/// schema backfill writes.
pub(super) fn new_uid() -> String {
    format!("{:032x}", rand::random::<u128>())
}

/// Record a local change to memory `id` so the next push uploads it.
///
/// The row uploads to its owner. An unowned row (written while logged out) is
/// claimed by the account logged in now; with nobody logged in it stays
/// unowned until the sync bootstrap claims it on the next logged-in command.
pub(super) fn touch(conn: &rusqlite::Connection, id: i64) -> Result<()> {
    touch_as(conn, id, crate::broker::current_user_id().as_deref())
}

/// [`touch`] with the logged-in account passed in.
pub(super) fn touch_as(conn: &rusqlite::Connection, id: i64, account: Option<&str>) -> Result<()> {
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT uid, sync_owner FROM memory_events WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    // Gone, or no uid yet. The schema backfill gives it one on the next open,
    // and the bootstrap claims it from there.
    let Some((Some(uid), owner)) = row else {
        return Ok(());
    };
    let owner = match (
        owner.filter(|o| !o.is_empty()),
        account.filter(|a| !a.is_empty()),
    ) {
        (Some(owner), _) => owner,
        (None, Some(account)) => {
            conn.execute(
                "UPDATE memory_events SET sync_owner = ?2 WHERE id = ?1",
                params![id, account],
            )?;
            account.to_string()
        }
        (None, None) => return Ok(()),
    };
    crate::broker::mark_dirty(conn, &owner, "memory", &uid, false)?;
    Ok(())
}

/// Delete memory `id` and record a tombstone for its owner, so the delete
/// reaches the account's other devices. Returns whether there was a row.
pub(super) fn delete_memory(conn: &rusqlite::Connection, id: i64) -> Result<bool> {
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT uid, sync_owner FROM memory_events WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((uid, owner)) = row else {
        return Ok(false);
    };
    conn.execute("DELETE FROM memory_events WHERE id = ?1", [id])?;
    // An unowned row never left this device, so there is nothing to tell the
    // others.
    if let (Some(uid), Some(owner)) = (uid, owner.filter(|o| !o.is_empty())) {
        crate::broker::mark_dirty(conn, &owner, "memory", &uid, true)?;
    }
    Ok(true)
}

/// The synced content of one memory row, as a sync record carries it.
#[derive(serde::Serialize, serde::Deserialize)]
struct MemoryRecord {
    project: String,
    event_type: String,
    scope: String,
    summary: String,
    confidence: f64,
    #[serde(default)]
    tags: Vec<String>,
    trigger_kind: String,
    source_kind: String,
    #[serde(default)]
    reinforcement_count: i64,
    #[serde(default)]
    last_reinforced_at: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

/// The sync payload (plaintext JSON) for the memory named `uid`, or `None` if
/// it no longer exists.
pub(crate) fn sync_payload(conn: &rusqlite::Connection, uid: &str) -> Result<Option<String>> {
    let record = conn
        .query_row(
            "SELECT project, event_type, scope, summary, confidence, tags_json, trigger_kind,
                    source_kind, reinforcement_count, last_reinforced_at, created_at, updated_at
               FROM memory_events WHERE uid = ?1",
            [uid],
            |r| {
                Ok(MemoryRecord {
                    project: r.get(0)?,
                    event_type: r.get(1)?,
                    scope: r.get(2)?,
                    summary: r.get(3)?,
                    confidence: r.get(4)?,
                    tags: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
                    trigger_kind: r.get(6)?,
                    source_kind: r.get(7)?,
                    reinforcement_count: r.get(8)?,
                    last_reinforced_at: r.get(9)?,
                    created_at: r.get(10)?,
                    updated_at: r.get(11)?,
                })
            },
        )
        .optional()?;
    record
        .map(|r| serde_json::to_string(&r).map_err(Into::into))
        .transpose()
}

/// Adopt a memory pulled from `account`: insert it, or bring the local copy up
/// to date. Local supersede links and reinforcement are left alone, and a row
/// owned by a different account is never overwritten.
pub(crate) fn apply_synced(
    conn: &rusqlite::Connection,
    account: &str,
    uid: &str,
    payload: &str,
) -> Result<()> {
    let record: MemoryRecord =
        serde_json::from_str(payload).context("invalid memory sync payload")?;
    conn.execute(
        "INSERT INTO memory_events (
            uid, sync_owner, project, event_type, scope, summary, summary_norm, confidence,
            tags_json, supersedes_json, trigger_kind, source_kind, last_reinforced_at,
            reinforcement_count, summary_hash, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, '[]', ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT(uid) DO UPDATE SET
            sync_owner = ?2, project = ?3, event_type = ?4, scope = ?5, summary = ?6,
            summary_norm = ?7, confidence = ?8, tags_json = ?9, trigger_kind = ?10,
            source_kind = ?11, summary_hash = ?14, created_at = ?15, updated_at = ?16
          WHERE memory_events.sync_owner IS NULL OR memory_events.sync_owner = ?2",
        params![
            uid,
            account,
            record.project,
            record.event_type,
            record.scope,
            record.summary,
            normalize_summary(&record.summary),
            record.confidence,
            serde_json::to_string(&record.tags)?,
            record.trigger_kind,
            record.source_kind,
            record.last_reinforced_at,
            record.reinforcement_count,
            summary_hash(&record.summary),
            record.created_at,
            record.updated_at,
        ],
    )?;
    Ok(())
}

/// Apply a tombstone pulled from `account`: delete the memory named `uid` if
/// it belongs to that account.
pub(crate) fn delete_synced(conn: &rusqlite::Connection, account: &str, uid: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM memory_events WHERE uid = ?1 AND (sync_owner IS NULL OR sync_owner = ?2)",
        params![uid, account],
    )?;
    Ok(())
}

/// Claim every unowned memory for `account` and return their uids. These are
/// rows written while nobody was logged in; they upload to whoever logs in.
///
/// Runs on every logged-in command, so a read guards the write: with nothing
/// to claim, which is the usual case, it never takes the write lock.
pub(crate) fn claim_unowned(conn: &rusqlite::Connection, account: &str) -> Result<Vec<String>> {
    let any: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory_events WHERE sync_owner IS NULL AND uid IS NOT NULL)",
        [],
        |r| r.get(0),
    )?;
    if !any {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "UPDATE memory_events SET sync_owner = ?1
          WHERE sync_owner IS NULL AND uid IS NOT NULL
          RETURNING uid",
    )?;
    let uids = stmt
        .query_map([account], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(uids)
}

/// The uids of every memory that syncs to `account`.
pub(crate) fn owned_uids(conn: &rusqlite::Connection, account: &str) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT uid FROM memory_events WHERE sync_owner = ?1 AND uid IS NOT NULL")?;
    let uids = stmt
        .query_map([account], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(uids)
}

#[cfg(test)]
mod tests;
