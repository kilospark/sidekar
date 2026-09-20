use super::*;

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
    let uid = current_user_id().unwrap_or_default();

    // `ensure_local_key` means there is always a key by this point, even
    // before login, so there is no plaintext fallback left to take.
    ensure_local_key().context("failed to establish a local encryption key")?;
    let secret_to_store =
        encrypt(secret).context("encryption key is loaded but encrypting the secret failed")?;

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
    let uid = current_user_id().unwrap_or_default();
    let mut stmt = conn.prepare(
        "SELECT id, service, account, secret, algorithm, digits, period, created_at \
         FROM totp_secrets WHERE user_id = ?1 ORDER BY service, account",
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
    let uid = current_user_id().unwrap_or_default();
    let mut stmt = conn.prepare(
        "SELECT id, service, account, secret, algorithm, digits, period, created_at \
         FROM totp_secrets WHERE user_id = ?1 AND service = ?2 AND account = ?3",
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
    let uid = current_user_id().unwrap_or_default();
    conn.execute(
        "DELETE FROM totp_secrets WHERE id = ?1 AND user_id = ?2",
        params![id, uid],
    )?;
    Ok(())
}

/// After a login transitions the account id (pre-login `''` -> the logged
/// in account, or one account -> another), move that uid's secrets onto
/// the new one and make sure nothing left under the new uid is plaintext.
/// Called once per transition from `fetch_encryption_key`.
pub fn migrate_totp_login_transition(old_uid: &str, new_uid: &str) -> Result<()> {
    if old_uid == new_uid || new_uid.is_empty() {
        return Ok(());
    }
    // Wrapped in one transaction so a crash mid-migration can't leave some
    // secrets moved (or re-keyed) and others not.
    let mut conn = open()?;
    let tx = conn.transaction()?;
    migrate_totp_rows(&tx, old_uid, new_uid)?;
    reencrypt_plaintext_secrets(&tx, new_uid)?;
    tx.commit()?;
    Ok(())
}

fn migrate_totp_rows(conn: &Connection, old_uid: &str, new_uid: &str) -> Result<()> {
    // See the matching comment in kv_store::migrate_kv_rows: rows under
    // `old_uid` are ciphertext under the persisted local key, which is no
    // longer the active key by the time this runs.
    let old_key = super::encryption::read_persisted_local_key(conn)?;
    let active_key =
        get_encryption_key().context("no active encryption key during login migration")?;

    let rows: Vec<(i64, String, String, String)> = {
        let mut stmt = conn
            .prepare("SELECT id, service, account, secret FROM totp_secrets WHERE user_id = ?1")?;
        let mut out = Vec::new();
        let mut rs = stmt.query(params![old_uid])?;
        while let Some(row) = rs.next()? {
            out.push((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?));
        }
        out
    };

    for (id, service, account, secret) in rows {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM totp_secrets \
             WHERE user_id = ?1 AND service = ?2 AND account = ?3)",
            params![new_uid, service, account],
            |r| r.get(0),
        )?;
        if exists {
            // The account already has this service/account pair from
            // another device. There is no TOTP history table to archive
            // the orphaned pre-login row into, so log and drop it rather
            // than fail the whole login on a UNIQUE(user_id, service, account)
            // conflict.
            crate::broker::try_log_event(
                "warn",
                "totp",
                "dropped an orphaned pre-login secret that collided with an account secret",
                Some(&format!("service={service} account={account}")),
            );
            conn.execute("DELETE FROM totp_secrets WHERE id = ?1", params![id])?;
        } else {
            let rekeyed = rekey_migrated_secret(&secret, old_key.as_deref(), &active_key)?;
            conn.execute(
                "UPDATE totp_secrets SET user_id = ?1, secret = ?2 WHERE id = ?3",
                params![new_uid, rekeyed, id],
            )?;
        }
    }
    Ok(())
}

/// See `kv_store::rekey_migrated_value` -- same re-key-or-pass-through logic
/// for TOTP secrets.
fn rekey_migrated_secret(
    secret: &str,
    old_key: Option<&[u8]>,
    active_key: &[u8],
) -> Result<String> {
    if !is_encrypted(secret) {
        return Ok(secret.to_string());
    }
    let old_key = old_key.context(
        "found an encrypted pre-login totp secret but no persisted local key to decrypt it with",
    )?;
    super::encryption::rekey_value(secret, old_key, active_key)
}

/// Re-encrypt any secret under `uid` that is still plaintext.
fn reencrypt_plaintext_secrets(conn: &Connection, uid: &str) -> Result<()> {
    let ids: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT id, secret FROM totp_secrets WHERE user_id = ?1")?;
        let mut out = Vec::new();
        let mut rows = stmt.query(params![uid])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let secret: String = row.get(1)?;
            if !is_encrypted(&secret) {
                out.push(id);
            }
        }
        out
    };
    if ids.is_empty() {
        return Ok(());
    }

    ensure_local_key().context("failed to establish a local encryption key")?;
    for id in ids {
        let secret: String = conn.query_row(
            "SELECT secret FROM totp_secrets WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        if is_encrypted(&secret) {
            continue; // migrated to ciphertext by an earlier pass
        }
        let encrypted = encrypt(&secret).context("failed to re-encrypt a plaintext totp secret")?;
        conn.execute(
            "UPDATE totp_secrets SET secret = ?1 WHERE id = ?2",
            params![encrypted, id],
        )?;
    }
    Ok(())
}
