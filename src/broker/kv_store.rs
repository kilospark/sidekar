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
    pub value: String,
    pub tags: Vec<String>,
    pub archived_at: u64,
}

fn parse_tags_json(s: &str) -> Vec<String> {
    serde_json::from_str(s).unwrap_or_default()
}

fn tags_to_json(tags: &[String]) -> String {
    serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string())
}

fn read_kv_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<KvEntry> {
    let value: String = row.get(2)?;
    let decrypted = if is_encrypted(&value) {
        decrypt(&value).unwrap_or(value)
    } else {
        value
    };
    let tags_raw: String = row.get(3)?;
    Ok(KvEntry {
        id: row.get(0)?,
        key: row.get(1)?,
        value: decrypted,
        tags: parse_tags_json(&tags_raw),
        created_at: row.get::<_, i64>(4)? as u64,
        updated_at: row.get::<_, i64>(5)? as u64,
    })
}

/// Archive current value to kv_history before overwrite. Keeps last 10 versions.
fn kv_archive(conn: &Connection, uid: &str, key: &str) -> Result<()> {
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

/// Set a KV value, scoped to current user. Archives previous value.
pub fn kv_set(key: &str, value: &str, tags: Option<&[String]>) -> Result<()> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    let uid = current_user_id().unwrap_or_default();

    // Archive existing value before overwrite
    kv_archive(&conn, &uid, key)?;

    let value_to_store = if get_encryption_key().is_some() {
        match encrypt(value) {
            Ok(enc) => enc,
            Err(e) => {
                eprintln!(
                    "Warning: encryption key available but encrypt failed: {}. Storing plaintext.",
                    e
                );
                value.to_string()
            }
        }
    } else {
        value.to_string()
    };

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
    Ok(())
}

/// Get a KV value, scoped to current user.
pub fn kv_get(key: &str) -> Result<Option<KvEntry>> {
    let conn = open()?;
    let uid = current_user_id().unwrap_or_default();

    conn.prepare(
        "SELECT id, key, value, tags, created_at, updated_at FROM kv_store \
         WHERE user_id = ?1 AND key = ?2",
    )?
    .query_row(params![uid, key], read_kv_entry)
    .optional()
    .map_err(Into::into)
}

/// List all KV entries for current user. Optionally filter by tag.
pub fn kv_list(filter_tag: Option<&str>) -> Result<Vec<KvEntry>> {
    let conn = open()?;
    let uid = current_user_id().unwrap_or_default();

    let mut stmt = conn.prepare(
        "SELECT id, key, value, tags, created_at, updated_at FROM kv_store \
         WHERE user_id = ?1 ORDER BY key",
    )?;
    let mut out = Vec::new();
    let mut rows = stmt.query(params![uid])?;
    while let Some(row) = rows.next()? {
        let entry = read_kv_entry(row)?;
        if let Some(tag) = filter_tag
            && !entry.tags.iter().any(|t| t == tag)
        {
            continue;
        }
        out.push(entry);
    }
    Ok(out)
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
        let decrypted = if is_encrypted(&value) {
            decrypt(&value).unwrap_or(value)
        } else {
            value
        };
        let tags_raw: String = row.get(2)?;
        out.push(KvHistoryEntry {
            version: row.get(0)?,
            value: decrypted,
            tags: parse_tags_json(&tags_raw),
            archived_at: row.get::<_, i64>(3)? as u64,
        });
    }
    Ok(out)
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
    Ok(())
}

/// Get all KV entries matching given keys or tags (for exec injection).
pub fn kv_get_for_exec(keys: &[String], filter_tag: Option<&str>) -> Result<Vec<KvEntry>> {
    let conn = open()?;
    let uid = current_user_id().unwrap_or_default();

    if !keys.is_empty() {
        // Fetch specific keys
        let mut out = Vec::new();
        for key in keys {
            let entry = conn
                .prepare(
                    "SELECT id, key, value, tags, created_at, updated_at FROM kv_store \
                     WHERE user_id = ?1 AND key = ?2",
                )?
                .query_row(params![uid, key], read_kv_entry)
                .optional()?
                .ok_or_else(|| anyhow!("Key '{}' not found", key))?;
            out.push(entry);
        }
        Ok(out)
    } else if let Some(tag) = filter_tag {
        // Fetch by tag
        kv_list(Some(tag))
    } else {
        // All secrets
        kv_list(None)
    }
}
