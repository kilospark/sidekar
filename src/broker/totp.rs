use super::*;

/// A one-time-password secret: time-based (TOTP), or counter-based (HOTP,
/// e.g. Duo).
#[derive(Debug, Clone)]
pub struct TotpSecret {
    pub id: i64,
    pub service: String,
    pub account: String,
    pub secret: String,
    pub algorithm: String,
    pub digits: i32,
    /// TOTP's step in seconds; unused for HOTP.
    pub period: i32,
    pub created_at: u64,
    /// `totp` or `hotp`.
    pub kind: String,
    /// HOTP: the counter the next code is made from. Every code issued
    /// advances it, so no code is ever handed out twice.
    pub counter: u64,
}

pub const KIND_TOTP: &str = "totp";
pub const KIND_HOTP: &str = "hotp";

/// The sync kind a secret travels under. HOTP has its own, so a sidekar
/// that predates HOTP rejects the record instead of storing it as TOTP and
/// producing wrong codes from it.
pub(crate) fn sync_kind(kind: &str) -> &'static str {
    if kind == KIND_HOTP { KIND_HOTP } else { KIND_TOTP }
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
    otp_add(service, account, secret, algorithm, digits, period, KIND_TOTP, 0)
}

/// Add an HOTP secret whose next code is made from `counter`.
pub fn hotp_add(
    service: &str,
    account: &str,
    secret: &str,
    algorithm: &str,
    digits: i32,
    counter: u64,
) -> Result<i64> {
    otp_add(service, account, secret, algorithm, digits, 30, KIND_HOTP, counter)
}

#[allow(clippy::too_many_arguments)]
fn otp_add(
    service: &str,
    account: &str,
    secret: &str,
    algorithm: &str,
    digits: i32,
    period: i32,
    kind: &str,
    counter: u64,
) -> Result<i64> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    let uid = current_user_id().unwrap_or_default();

    // `ensure_local_key` means there is always a key by this point, even
    // before login, so there is no plaintext fallback left to take.
    ensure_local_key().context("failed to establish a local encryption key")?;
    let secret_to_store =
        encrypt(secret).context("encryption key is loaded but encrypting the secret failed")?;

    let previous_kind: Option<String> = conn
        .query_row(
            "SELECT kind FROM totp_secrets WHERE user_id = ?1 AND service = ?2 AND account = ?3",
            params![uid, service, account],
            |r| r.get(0),
        )
        .optional()?;
    conn.execute(
        "INSERT INTO totp_secrets (user_id, service, account, secret, algorithm, digits, period, created_at, kind, counter) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
         ON CONFLICT(user_id, service, account) DO UPDATE SET secret = ?4, algorithm = ?5, digits = ?6, period = ?7, kind = ?9, counter = ?10",
        params![uid, service, account, secret_to_store, algorithm, digits, period, now, kind, counter as i64],
    )?;
    let record_id = super::sync::totp_record_id(service, account);
    // Replacing a TOTP secret with an HOTP one, or the reverse, moves it to
    // the other sync kind: the old kind's record is deleted, or other
    // devices would keep it.
    if let Some(old) = previous_kind
        && sync_kind(&old) != sync_kind(kind)
    {
        super::sync::mark_dirty(&conn, &uid, sync_kind(&old), &record_id, true)?;
    }
    super::sync::mark_dirty(&conn, &uid, sync_kind(kind), &record_id, false)?;
    Ok(conn.last_insert_rowid())
}

/// Issue the next HOTP code's counter: the current one, after advancing the
/// stored counter past it, in one transaction so two callers never get the
/// same. `None` when there is no HOTP secret by that name.
pub fn hotp_take(service: &str, account: &str) -> Result<Option<(TotpSecret, u64)>> {
    let mut conn = open()?;
    let uid = current_user_id().unwrap_or_default();
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let Some(rec) = select_one(&tx, &uid, service, account)? else {
        return Ok(None);
    };
    if rec.kind != KIND_HOTP {
        return Ok(None);
    }
    let used = rec.counter;
    tx.execute(
        "UPDATE totp_secrets SET counter = ?4 WHERE user_id = ?1 AND service = ?2 AND account = ?3",
        params![uid, service, account, (used + 1) as i64],
    )?;
    let record_id = super::sync::totp_record_id(service, account);
    super::sync::mark_dirty(&tx, &uid, KIND_HOTP, &record_id, false)?;
    tx.commit()?;
    Ok(Some((rec, used)))
}

/// Set the counter the next HOTP code is made from, to resynchronise with
/// a server that has moved ahead.
pub fn hotp_set_counter(service: &str, account: &str, counter: u64) -> Result<bool> {
    let conn = open()?;
    let uid = current_user_id().unwrap_or_default();
    let n = conn.execute(
        "UPDATE totp_secrets SET counter = ?4 WHERE user_id = ?1 AND service = ?2 AND account = ?3 AND kind = 'hotp'",
        params![uid, service, account, counter as i64],
    )?;
    if n > 0 {
        let record_id = super::sync::totp_record_id(service, account);
        super::sync::mark_dirty(&conn, &uid, KIND_HOTP, &record_id, false)?;
    }
    Ok(n > 0)
}

const SELECT_COLUMNS: &str =
    "id, service, account, secret, algorithm, digits, period, created_at, kind, counter";

fn row_to_secret(row: &rusqlite::Row<'_>) -> rusqlite::Result<TotpSecret> {
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
        kind: row.get(8)?,
        counter: row.get::<_, i64>(9)?.max(0) as u64,
    })
}

fn select_one(conn: &Connection, uid: &str, service: &str, account: &str) -> Result<Option<TotpSecret>> {
    conn.prepare(&format!(
        "SELECT {SELECT_COLUMNS} FROM totp_secrets WHERE user_id = ?1 AND service = ?2 AND account = ?3"
    ))?
    .query_row(params![uid, service, account], row_to_secret)
    .optional()
    .map_err(Into::into)
}

/// List all TOTP secrets for current user.
pub fn totp_list() -> Result<Vec<TotpSecret>> {
    let conn = open()?;
    let uid = current_user_id().unwrap_or_default();
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLUMNS} FROM totp_secrets WHERE user_id = ?1 ORDER BY service, account"
    ))?;
    let rows = stmt.query_map(params![uid], row_to_secret)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Get a one-time-password secret for a service+account, scoped to current user.
pub fn totp_get(service: &str, account: &str) -> Result<Option<TotpSecret>> {
    let conn = open()?;
    let uid = current_user_id().unwrap_or_default();
    select_one(&conn, &uid, service, account)
}

/// Delete a TOTP secret (by id — already scoped by user via query)
pub fn totp_delete(id: i64) -> Result<()> {
    let conn = open()?;
    let uid = current_user_id().unwrap_or_default();

    // Look up service/account before the row is gone: sync needs them to
    // build the tombstone's record_id, and there's nowhere else to recover
    // them from once the DELETE below runs (unlike kv, totp has no history
    // table to fall back on).
    let found: Option<(String, String, String)> = conn
        .prepare("SELECT service, account, kind FROM totp_secrets WHERE id = ?1 AND user_id = ?2")?
        .query_row(params![id, uid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;

    conn.execute(
        "DELETE FROM totp_secrets WHERE id = ?1 AND user_id = ?2",
        params![id, uid],
    )?;

    if let Some((service, account, kind)) = found {
        let record_id = super::sync::totp_record_id(&service, &account);
        super::sync::mark_dirty(&conn, &uid, sync_kind(&kind), &record_id, true)?;
    }
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
    let mut warnings = Vec::new();
    let mut conn = open()?;
    let tx = conn.transaction()?;
    migrate_totp_rows(&tx, old_uid, new_uid, &mut warnings)?;
    reencrypt_plaintext_secrets(&tx, new_uid)?;
    tx.commit()?;

    // Logged after the transaction commits, not from inside it: see the
    // matching comment in kv_store::migrate_kv_login_transition.
    for (service, account, reason) in warnings {
        crate::broker::try_log_event(
            "warn",
            "totp",
            &reason,
            Some(&format!("service={service} account={account}")),
        );
    }
    Ok(())
}

fn migrate_totp_rows(
    conn: &Connection,
    old_uid: &str,
    new_uid: &str,
    warnings: &mut Vec<(String, String, String)>,
) -> Result<()> {
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
            warnings.push((
                service.clone(),
                account.clone(),
                "dropped an orphaned pre-login secret that collided with an account secret"
                    .to_string(),
            ));
            conn.execute("DELETE FROM totp_secrets WHERE id = ?1", params![id])?;
        } else {
            // A stranded secret (the key that protected it is gone, or the
            // ciphertext otherwise can't be decrypted) must not fail the
            // whole login: leave it in place under `old_uid` and move on
            // (see the matching comment in kv_store::migrate_kv_rows).
            let rekeyed = match rekey_migrated_secret(&secret, old_key.as_deref(), &active_key) {
                Ok(v) => v,
                Err(e) => {
                    warnings.push((
                        service.clone(),
                        account.clone(),
                        format!(
                            "could not decrypt a pre-login totp secret during login migration; leaving it stranded: {e:#}"
                        ),
                    ));
                    continue;
                }
            };
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
