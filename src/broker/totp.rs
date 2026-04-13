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

    let secret_to_store = if get_encryption_key().is_some() {
        match encrypt(secret) {
            Ok(enc) => enc,
            Err(e) => {
                crate::broker::try_log_event(
                    "warn",
                    "totp",
                    "encryption key available but encrypt failed; storing plaintext",
                    Some(&format!("{e:#}")),
                );
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
