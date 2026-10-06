use super::*;

/// Get a stored auth value (e.g., "token", "created_at"), or `None` if none
/// is stored.
///
/// `config_get` answers a key it has no row for with an empty string, so empty
/// means absent. Taking it as a value made every logged-out machine look logged
/// in: each command fetched the encryption key with an empty bearer token and
/// warned about the 401.
pub fn auth_get(key: &str) -> Option<String> {
    Some(crate::config::config_get(&format!("auth:{key}"))).filter(|v| !v.is_empty())
}

/// Set an auth value.
pub fn auth_set(key: &str, value: &str) -> Result<()> {
    crate::config::config_set(&format!("auth:{key}"), value)
}

/// Delete an auth value.
pub fn auth_delete(key: &str) -> Result<()> {
    crate::config::config_delete(&format!("auth:{key}"))
}

/// Clear all auth data (for logout).
pub fn auth_clear() -> Result<()> {
    let conn = open()?;
    conn.execute("DELETE FROM config WHERE key LIKE 'auth:%'", [])?;
    Ok(())
}
