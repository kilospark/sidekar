use super::*;

/// Get a stored auth value (e.g., "token", "created_at").
pub fn auth_get(key: &str) -> Option<String> {
    crate::config::config_get(&format!("auth:{key}")).into()
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
