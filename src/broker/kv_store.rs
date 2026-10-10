use super::*;

/// KV store record
#[derive(Debug, Clone)]
pub struct KvEntry {
    pub id: i64,
    pub key: String,
    pub value: String,
    pub tags: Vec<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

/// KV history record
#[derive(Debug, Clone)]
pub struct KvHistoryEntry {
    pub version: i64,
    /// The archived value, or why it can't be decrypted.
    pub value: std::result::Result<String, String>,
    pub tags: Vec<String>,
    pub archived_at: u64,
}

/// A kv row whose value can't be decrypted. Its key and tags are stored in
/// the clear, so it can still be listed and deleted; the value is unusable
/// until it is set again, or the key it was encrypted under is loaded.
#[derive(Debug, Clone)]
pub struct KvUnreadable {
    pub key: String,
    pub tags: Vec<String>,
    /// Why the value can't be decrypted.
    pub reason: String,
}

/// What a kv listing found: the entries whose values could be read, and the
/// keys whose values could not.
#[derive(Debug, Clone, Default)]
pub struct KvListing {
    pub entries: Vec<KvEntry>,
    pub unreadable: Vec<KvUnreadable>,
}

impl KvListing {
    /// Every key with its tags, whether or not its value could be read, in
    /// key order.
    pub fn keys(&self) -> Vec<(&str, &[String])> {
        let readable = self
            .entries
            .iter()
            .map(|e| (e.key.as_str(), e.tags.as_slice()));
        let unreadable = self
            .unreadable
            .iter()
            .map(|u| (u.key.as_str(), u.tags.as_slice()));
        let mut keys: Vec<_> = readable.chain(unreadable).collect();
        keys.sort_by_key(|(key, _)| *key);
        keys
    }
}

/// The error for values that can't be decrypted, naming each key.
fn kv_unreadable_error(unreadable: &[KvUnreadable]) -> anyhow::Error {
    let keys: Vec<String> = unreadable.iter().map(|u| format!("'{}'", u.key)).collect();
    let reason = unreadable
        .first()
        .map(|u| u.reason.as_str())
        .unwrap_or_default();
    match keys.len() {
        1 => anyhow!("kv value {} can't be decrypted: {reason}", keys[0]),
        _ => anyhow!("kv values {} can't be decrypted: {reason}", keys.join(", ")),
    }
}

fn parse_tags_json(s: &str) -> Vec<String> {
    serde_json::from_str(s).unwrap_or_default()
}

fn tags_to_json(tags: &[String]) -> String {
    serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string())
}

const SELECT_KV: &str = "SELECT id, key, value, tags, created_at, updated_at FROM kv_store";

/// Read a row selected with [`SELECT_KV`]. Its value is as stored, possibly
/// still encrypted; [`open_kv`] decrypts it.
fn read_stored_kv(row: &rusqlite::Row<'_>) -> rusqlite::Result<KvEntry> {
    let tags_raw: String = row.get(3)?;
    Ok(KvEntry {
        id: row.get(0)?,
        key: row.get(1)?,
        value: row.get(2)?,
        tags: parse_tags_json(&tags_raw),
        created_at: row.get::<_, i64>(4)? as u64,
        updated_at: row.get::<_, i64>(5)? as u64,
    })
}

/// Decrypt a stored entry's value. One that can't be decrypted comes back as
/// [`KvUnreadable`]: returning the ciphertext as the value let callers use
/// `$encrypted$...` as the secret itself (#21).
fn open_kv(mut entry: KvEntry) -> std::result::Result<KvEntry, KvUnreadable> {
    match decrypt_stored(&entry.value) {
        Ok(value) => {
            entry.value = value;
            Ok(entry)
        }
        Err(e) => Err(KvUnreadable {
            key: entry.key,
            tags: entry.tags,
            reason: format!("{e:#}"),
        }),
    }
}

fn kv_lookup_in(
    conn: &Connection,
    uid: &str,
    key: &str,
) -> Result<Option<std::result::Result<KvEntry, KvUnreadable>>> {
    Ok(conn
        .prepare(&format!("{SELECT_KV} WHERE user_id = ?1 AND key = ?2"))?
        .query_row(params![uid, key], read_stored_kv)
        .optional()?
        .map(open_kv))
}

fn kv_get_in(conn: &Connection, uid: &str, key: &str) -> Result<Option<KvEntry>> {
    kv_lookup_in(conn, uid, key)?
        .map(|found| found.map_err(|u| kv_unreadable_error(&[u])))
        .transpose()
}

/// Archive current value to kv_history before overwrite. Keeps last 10 versions.
///
/// `pub(crate)` so `sync::apply_kv_remote` can archive a locally-overwritten
/// value the same way a local `kv_set` does when a remote-driven write wins
/// a merge.
pub(crate) fn kv_archive(conn: &Connection, uid: &str, key: &str) -> Result<()> {
    // Check if there's an existing value to archive
    let existing: Option<(String, String)> = conn
        .prepare("SELECT value, tags FROM kv_store WHERE user_id = ?1 AND key = ?2")?
        .query_row(params![uid, key], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;

    if let Some((old_value, old_tags)) = existing {
        let now = crate::message::epoch_secs() as i64;
        let next_version: i64 = conn
            .prepare(
                "SELECT COALESCE(MAX(version), 0) + 1 FROM kv_history \
                 WHERE user_id = ?1 AND key = ?2",
            )?
            .query_row(params![uid, key], |r| r.get(0))?;

        conn.execute(
            "INSERT INTO kv_history (user_id, key, version, value, tags, archived_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![uid, key, next_version, old_value, old_tags, now],
        )?;

        // Prune: keep last 10
        conn.execute(
            "DELETE FROM kv_history WHERE user_id = ?1 AND key = ?2 AND version NOT IN \
             (SELECT version FROM kv_history WHERE user_id = ?1 AND key = ?2 \
              ORDER BY version DESC LIMIT 10)",
            params![uid, key],
        )?;
    }
    Ok(())
}

/// Prefix of kv keys that hold per-device state and never sync: the
/// Anthropic provider's device id, a project's bus nickname, Gemini context
/// cache handles, and the refresh lease bookkeeping. Synced, every device
/// pushed its own value under the same key: devices adopted each other's
/// value (two machines' agents in one project took the same nick, which bus
/// sync then could not tell apart), and the losers' pushes were refused for
/// good.
pub(crate) const DEVICE_LOCAL_PREFIX: &str = "internal:";

/// Per-device keys from before they moved under [`DEVICE_LOCAL_PREFIX`], with
/// the prefix each moved to. `broker::migrate_device_local_kv_keys` renames
/// the local rows and tombstones the copies the server holds; a pulled one,
/// from a device still on an older build, is left out.
pub(crate) const LEGACY_DEVICE_LOCAL_PREFIXES: &[(&str, &str)] = &[
    ("_nick:", "internal:nick:"),
    ("gemini_cache:", "internal:gemini_cache:"),
];

/// Whether a kv key syncs across devices. See [`DEVICE_LOCAL_PREFIX`].
pub(crate) fn kv_key_syncs(key: &str) -> bool {
    !key.starts_with(DEVICE_LOCAL_PREFIX)
        && !LEGACY_DEVICE_LOCAL_PREFIXES
            .iter()
            .any(|(old, _)| key.starts_with(old))
}

/// Record a kv change for the next sync push, unless the key stays on this
/// device.
fn mark_kv_dirty(conn: &Connection, uid: &str, key: &str, deleted: bool) -> Result<()> {
    if kv_key_syncs(key) {
        super::sync::mark_dirty(conn, uid, "kv", key, deleted)?;
    }
    Ok(())
}

/// Set a KV value, scoped to current user. Archives previous value.
pub fn kv_set(key: &str, value: &str, tags: Option<&[String]>) -> Result<()> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    let uid = current_user_id().unwrap_or_default();

    // Encrypt before touching the row. A caller with a key has asked for the
    // value to be stored encrypted; falling back to plaintext on failure gave
    // them the success message anyway, so the only way to learn a secret was
    // sitting in the clear was to read the event log. Refuse instead, and
    // refuse before the archive step so a failed write leaves nothing moved.
    // `ensure_local_key` means there is always a key by this point, even
    // before login, so there is no plaintext fallback left to take.
    ensure_local_key().context("failed to establish a local encryption key")?;
    let value_to_store =
        encrypt(value).context("encryption key is loaded but encrypting the value failed")?;

    // Archive existing value before overwrite
    kv_archive(&conn, &uid, key)?;

    let tags_json = match tags {
        Some(t) => tags_to_json(t),
        None => {
            // Preserve existing tags on update if none specified
            conn.prepare("SELECT tags FROM kv_store WHERE user_id = ?1 AND key = ?2")?
                .query_row(params![uid, key], |r| r.get::<_, String>(0))
                .unwrap_or_else(|_| "[]".to_string())
        }
    };

    conn.execute(
        "INSERT INTO kv_store (user_id, key, value, tags, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(user_id, key) DO UPDATE SET value = ?3, tags = ?4, updated_at = ?6",
        params![uid, key, value_to_store, tags_json, now, now],
    )?;
    mark_kv_dirty(&conn, &uid, key, false)?;
    Ok(())
}

/// Get a KV value, scoped to current user. A value that can't be decrypted
/// is an error naming the key.
pub fn kv_get(key: &str) -> Result<Option<KvEntry>> {
    let conn = open()?;
    let uid = current_user_id().unwrap_or_default();
    kv_get_in(&conn, &uid, key)
}

/// Look up a KV key, scoped to current user, with a value that can't be
/// decrypted reported as [`KvUnreadable`] rather than as an error. For
/// callers that show the state, like `kv history`; the rest want [`kv_get`].
pub fn kv_lookup(key: &str) -> Result<Option<std::result::Result<KvEntry, KvUnreadable>>> {
    let conn = open()?;
    let uid = current_user_id().unwrap_or_default();
    kv_lookup_in(&conn, &uid, key)
}

/// Every KV row for the current user, optionally only those tagged
/// `filter_tag`: the entries whose values could be read, and the keys whose
/// values could not.
pub fn kv_scan(filter_tag: Option<&str>) -> Result<KvListing> {
    let conn = open()?;
    let uid = current_user_id().unwrap_or_default();

    let mut stmt = conn.prepare(&format!("{SELECT_KV} WHERE user_id = ?1 ORDER BY key"))?;
    let mut listing = KvListing::default();
    let mut rows = stmt.query(params![uid])?;
    while let Some(row) = rows.next()? {
        let entry = read_stored_kv(row)?;
        if let Some(tag) = filter_tag
            && !entry.tags.iter().any(|t| t == tag)
        {
            continue;
        }
        match open_kv(entry) {
            Ok(entry) => listing.entries.push(entry),
            Err(unreadable) => listing.unreadable.push(unreadable),
        }
    }
    Ok(listing)
}

/// The KV entries for the current user whose values can be read, optionally
/// only those tagged `filter_tag`. A row whose value can't be decrypted is
/// left out; [`kv_scan`] reports those.
pub fn kv_list(filter_tag: Option<&str>) -> Result<Vec<KvEntry>> {
    Ok(kv_scan(filter_tag)?.entries)
}

/// Delete a KV entry, scoped to current user.
pub fn kv_delete(key: &str) -> Result<()> {
    let conn = open()?;
    let uid = current_user_id().unwrap_or_default();
    conn.execute(
        "DELETE FROM kv_store WHERE user_id = ?1 AND key = ?2",
        params![uid, key],
    )?;
    conn.execute(
        "DELETE FROM kv_history WHERE user_id = ?1 AND key = ?2",
        params![uid, key],
    )?;
    mark_kv_dirty(&conn, &uid, key, true)?;
    Ok(())
}

/// Add tags to an existing KV entry.
pub fn kv_tag_add(key: &str, new_tags: &[String]) -> Result<()> {
    let conn = open()?;
    let uid = current_user_id().unwrap_or_default();

    let existing: String = conn
        .prepare("SELECT tags FROM kv_store WHERE user_id = ?1 AND key = ?2")?
        .query_row(params![uid, key], |r| r.get(0))
        .optional()?
        .ok_or_else(|| anyhow!("Key '{}' not found", key))?;

    let mut tags = parse_tags_json(&existing);
    for t in new_tags {
        if !tags.contains(t) {
            tags.push(t.clone());
        }
    }

    conn.execute(
        "UPDATE kv_store SET tags = ?1 WHERE user_id = ?2 AND key = ?3",
        params![tags_to_json(&tags), uid, key],
    )?;
    mark_kv_dirty(&conn, &uid, key, false)?;
    Ok(())
}

/// Remove tags from an existing KV entry.
pub fn kv_tag_remove(key: &str, rm_tags: &[String]) -> Result<()> {
    let conn = open()?;
    let uid = current_user_id().unwrap_or_default();

    let existing: String = conn
        .prepare("SELECT tags FROM kv_store WHERE user_id = ?1 AND key = ?2")?
        .query_row(params![uid, key], |r| r.get(0))
        .optional()?
        .ok_or_else(|| anyhow!("Key '{}' not found", key))?;

    let tags: Vec<String> = parse_tags_json(&existing)
        .into_iter()
        .filter(|t| !rm_tags.contains(t))
        .collect();

    conn.execute(
        "UPDATE kv_store SET tags = ?1 WHERE user_id = ?2 AND key = ?3",
        params![tags_to_json(&tags), uid, key],
    )?;
    mark_kv_dirty(&conn, &uid, key, false)?;
    Ok(())
}

/// Get version history for a KV key.
pub fn kv_history(key: &str) -> Result<Vec<KvHistoryEntry>> {
    let conn = open()?;
    let uid = current_user_id().unwrap_or_default();

    let mut stmt = conn.prepare(
        "SELECT version, value, tags, archived_at FROM kv_history \
         WHERE user_id = ?1 AND key = ?2 ORDER BY version DESC",
    )?;
    let mut out = Vec::new();
    let mut rows = stmt.query(params![uid, key])?;
    while let Some(row) = rows.next()? {
        let value: String = row.get(1)?;
        let tags_raw: String = row.get(2)?;
        out.push(KvHistoryEntry {
            version: row.get(0)?,
            value: decrypt_stored(&value).map_err(|e| format!("{e:#}")),
            tags: parse_tags_json(&tags_raw),
            archived_at: row.get::<_, i64>(3)? as u64,
        });
    }
    Ok(out)
}

/// Drop every archived version of a key, keeping its current value. For
/// keys whose old values are spent credentials (rotated OAuth tokens), where
/// history is only a pile of secrets with no use.
pub fn kv_clear_history(key: &str) -> Result<()> {
    let conn = open()?;
    let uid = current_user_id().unwrap_or_default();
    conn.execute(
        "DELETE FROM kv_history WHERE user_id = ?1 AND key = ?2",
        params![uid, key],
    )?;
    Ok(())
}

/// Rollback a KV key to a previous version. Current value is archived first (reversible).
pub fn kv_rollback(key: &str, target_version: i64) -> Result<()> {
    let conn = open()?;
    let uid = current_user_id().unwrap_or_default();

    // Fetch target version
    let (target_value, target_tags): (String, String) = conn
        .prepare(
            "SELECT value, tags FROM kv_history \
             WHERE user_id = ?1 AND key = ?2 AND version = ?3",
        )?
        .query_row(params![uid, key, target_version], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?
        .ok_or_else(|| anyhow!("Version {} not found for key '{}'", target_version, key))?;

    // Archive current value before rollback (so rollback is reversible)
    kv_archive(&conn, &uid, key)?;

    let now = crate::message::epoch_secs() as i64;
    conn.execute(
        "UPDATE kv_store SET value = ?1, tags = ?2, updated_at = ?3 \
         WHERE user_id = ?4 AND key = ?5",
        params![target_value, target_tags, now, uid, key],
    )?;
    mark_kv_dirty(&conn, &uid, key, false)?;
    Ok(())
}

/// Get all KV entries matching given keys or tags (for exec injection).
///
/// A value that can't be decrypted is an error, whether it was named or came
/// with the tag: running the command without it would fail somewhere less
/// obvious.
pub fn kv_get_for_exec(keys: &[String], filter_tag: Option<&str>) -> Result<Vec<KvEntry>> {
    if !keys.is_empty() {
        let conn = open()?;
        let uid = current_user_id().unwrap_or_default();
        return keys
            .iter()
            .map(|key| {
                kv_get_in(&conn, &uid, key)?.ok_or_else(|| anyhow!("Key '{}' not found", key))
            })
            .collect();
    }
    // By tag, or every key.
    let listing = kv_scan(filter_tag)?;
    if !listing.unreadable.is_empty() {
        return Err(kv_unreadable_error(&listing.unreadable));
    }
    Ok(listing.entries)
}

/// After a login transitions the account id (pre-login `''` -> the logged
/// in account, or one account -> another), move that uid's rows onto the
/// new one and make sure nothing left under the new uid is plaintext.
/// Called once per transition from `fetch_encryption_key`.
pub fn migrate_kv_login_transition(old_uid: &str, new_uid: &str) -> Result<()> {
    if old_uid == new_uid || new_uid.is_empty() {
        return Ok(());
    }
    // Wrapped in one transaction so a crash mid-migration can't leave some
    // rows moved (or re-keyed) and others not: either every row lands under
    // `new_uid` encrypted with the active key, or none of them do.
    let mut warnings = Vec::new();
    let mut conn = open()?;
    let tx = conn.transaction()?;
    migrate_kv_rows(&tx, old_uid, new_uid, &mut warnings)?;
    reencrypt_plaintext_rows(&tx, new_uid)?;
    tx.commit()?;

    // Logged after the transaction commits, not from inside it: `try_log_event`
    // opens its own connection, and writing through it while `tx` still holds
    // an open read snapshot on this one trips SQLite's WAL promote-to-write
    // check (`SQLITE_BUSY_SNAPSHOT`) as soon as `tx` tries its next write.
    for (key, reason) in warnings {
        crate::broker::try_log_event(
            "warn",
            "kv",
            "could not decrypt a pre-login kv row during login migration; leaving it stranded",
            Some(&format!("key={key}: {reason}")),
        );
    }
    Ok(())
}

/// Insert `value`/`tags` as the newest history entry for `uid`/`key`
/// without touching `kv_store` -- used when a migrated row collides with
/// one the account already has, so the orphaned value isn't silently
/// dropped even though it can't move into `kv_store` in place.
fn archive_value(conn: &Connection, uid: &str, key: &str, value: &str, tags: &str) -> Result<()> {
    let now = crate::message::epoch_secs() as i64;
    let next_version: i64 = conn
        .prepare(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM kv_history \
             WHERE user_id = ?1 AND key = ?2",
        )?
        .query_row(params![uid, key], |r| r.get(0))?;
    conn.execute(
        "INSERT INTO kv_history (user_id, key, version, value, tags, archived_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![uid, key, next_version, value, tags, now],
    )?;
    Ok(())
}

/// Internal rows this crate regenerates on demand if lost, so dropping a
/// stranded (undecryptable) copy during login migration is safe. See
/// `providers::anthropic::get_or_create_device_id`.
const REGENERABLE_KEYS: &[&str] = &["internal:device_id"];

fn is_regenerable_key(key: &str) -> bool {
    REGENERABLE_KEYS.contains(&key)
}

fn migrate_kv_rows(
    conn: &Connection,
    old_uid: &str,
    new_uid: &str,
    warnings: &mut Vec<(String, String)>,
) -> Result<()> {
    // Rows under `old_uid` are ciphertext under whatever key was active when
    // they were written -- pre-login, that's always the persisted local key
    // (`kv_set` runs `ensure_local_key` first). `fetch_encryption_key`
    // installs the account key as active *before* calling this migration,
    // so without re-keying here those rows would sit under a key nothing
    // still holds, and every read of them would fail.
    let old_key = super::encryption::read_persisted_local_key(conn)?;
    let active_key =
        get_encryption_key().context("no active encryption key during login migration")?;

    let rows: Vec<(String, String, String)> = {
        let mut stmt = conn.prepare("SELECT key, value, tags FROM kv_store WHERE user_id = ?1")?;
        let mut out = Vec::new();
        let mut rs = stmt.query(params![old_uid])?;
        while let Some(row) = rs.next()? {
            out.push((row.get(0)?, row.get(1)?, row.get(2)?));
        }
        out
    };

    for (key, value, tags) in rows {
        // A stranded row (the key that protected it is gone, or the
        // ciphertext otherwise can't be decrypted) must not fail the whole
        // login: drop it if it's known-regenerable, otherwise leave it in
        // place under `old_uid` and move on to the rest of the migration.
        let rekeyed = match rekey_migrated_value(&value, old_key.as_deref(), &active_key) {
            Ok(v) => v,
            Err(e) => {
                warnings.push((key.clone(), format!("{e:#}")));
                if is_regenerable_key(&key) {
                    conn.execute(
                        "DELETE FROM kv_store WHERE user_id = ?1 AND key = ?2",
                        params![old_uid, key],
                    )?;
                }
                continue;
            }
        };
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM kv_store WHERE user_id = ?1 AND key = ?2)",
            params![new_uid, key],
            |r| r.get(0),
        )?;
        if exists {
            // The account already has this key (e.g. synced from another
            // device); UNIQUE(user_id, key) rules out moving this row in
            // place, so archive it into history instead of dropping it.
            archive_value(conn, new_uid, &key, &rekeyed, &tags)?;
            conn.execute(
                "DELETE FROM kv_store WHERE user_id = ?1 AND key = ?2",
                params![old_uid, key],
            )?;
        } else {
            conn.execute(
                "UPDATE kv_store SET user_id = ?1, value = ?2 WHERE user_id = ?3 AND key = ?4",
                params![new_uid, rekeyed, old_uid, key],
            )?;
        }
    }

    migrate_kv_history(
        conn,
        old_uid,
        new_uid,
        old_key.as_deref(),
        &active_key,
        warnings,
    )
}

/// Re-key a value being moved off the pre-login uid: ciphertext is
/// decrypted with the persisted local key it was written under and
/// re-encrypted with the now-active account key. Legacy plaintext (never
/// encrypted at all) passes through unchanged for `reencrypt_plaintext_rows`
/// to pick up afterward.
fn rekey_migrated_value(value: &str, old_key: Option<&[u8]>, active_key: &[u8]) -> Result<String> {
    if !is_encrypted(value) {
        return Ok(value.to_string());
    }
    let old_key = old_key.context(
        "found an encrypted pre-login kv row but no persisted local key to decrypt it with",
    )?;
    super::encryption::rekey_value(value, old_key, active_key)
}

/// Move every history row for `old_uid` onto `new_uid`, renumbering
/// versions so they land after whatever history the account already has
/// for that key (version numbers are only ever compared within a uid).
fn migrate_kv_history(
    conn: &Connection,
    old_uid: &str,
    new_uid: &str,
    old_key: Option<&[u8]>,
    active_key: &[u8],
    warnings: &mut Vec<(String, String)>,
) -> Result<()> {
    let rows: Vec<(i64, String, String, String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT id, key, value, tags, archived_at FROM kv_history \
             WHERE user_id = ?1 ORDER BY key, version",
        )?;
        let mut out = Vec::new();
        let mut rs = stmt.query(params![old_uid])?;
        while let Some(row) = rs.next()? {
            out.push((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get::<_, i64>(4)?,
            ));
        }
        out
    };

    for (id, key, value, tags, archived_at) in rows {
        let rekeyed = match rekey_migrated_value(&value, old_key, active_key) {
            Ok(v) => v,
            Err(e) => {
                warnings.push((key.clone(), format!("{e:#}")));
                if is_regenerable_key(&key) {
                    conn.execute("DELETE FROM kv_history WHERE id = ?1", params![id])?;
                }
                continue;
            }
        };
        let next_version: i64 = conn
            .prepare(
                "SELECT COALESCE(MAX(version), 0) + 1 FROM kv_history \
                 WHERE user_id = ?1 AND key = ?2",
            )?
            .query_row(params![new_uid, key], |r| r.get(0))?;
        conn.execute(
            "INSERT INTO kv_history (user_id, key, version, value, tags, archived_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![new_uid, key, next_version, rekeyed, tags, archived_at],
        )?;
        conn.execute("DELETE FROM kv_history WHERE id = ?1", params![id])?;
    }
    Ok(())
}

/// Re-encrypt any row under `uid` that is still plaintext, archiving the
/// current value first so rollback still works.
fn reencrypt_plaintext_rows(conn: &Connection, uid: &str) -> Result<()> {
    let keys: Vec<String> = {
        let mut stmt = conn.prepare("SELECT key, value FROM kv_store WHERE user_id = ?1")?;
        let mut out = Vec::new();
        let mut rows = stmt.query(params![uid])?;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let value: String = row.get(1)?;
            if !is_encrypted(&value) {
                out.push(key);
            }
        }
        out
    };
    if keys.is_empty() {
        return Ok(());
    }

    ensure_local_key().context("failed to establish a local encryption key")?;
    let now = crate::message::epoch_secs() as i64;
    for key in keys {
        let value: String = conn.query_row(
            "SELECT value FROM kv_store WHERE user_id = ?1 AND key = ?2",
            params![uid, key],
            |r| r.get(0),
        )?;
        if is_encrypted(&value) {
            continue; // migrated to ciphertext by an earlier pass over the same key
        }
        kv_archive(conn, uid, &key)?;
        let encrypted = encrypt(&value).context("failed to re-encrypt a plaintext kv row")?;
        conn.execute(
            "UPDATE kv_store SET value = ?1, updated_at = ?2 WHERE user_id = ?3 AND key = ?4",
            params![encrypted, now, uid, key],
        )?;
    }
    Ok(())
}
