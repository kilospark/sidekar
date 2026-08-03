use super::*;

/// One row of the `prompts` table.
///
/// `default_hash` records which shipped default this row was derived
/// from. For an unedited row it drives the refresh check on upgrade; for
/// an edited row it is how `prompts::is_drifted` notices that the
/// shipped default moved on after the user customized the text.
#[derive(Debug, Clone)]
pub struct PromptRow {
    pub key: String,
    pub value: String,
    pub default_hash: String,
    pub edited: bool,
    pub updated_at: u64,
}

fn read_prompt_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PromptRow> {
    Ok(PromptRow {
        key: row.get(0)?,
        value: row.get(1)?,
        default_hash: row.get(2)?,
        edited: row.get::<_, i64>(3)? != 0,
        updated_at: row.get::<_, i64>(4)? as u64,
    })
}

pub fn prompt_get(key: &str) -> Result<Option<PromptRow>> {
    let conn = open()?;
    conn.prepare("SELECT key, value, default_hash, edited, updated_at FROM prompts WHERE key = ?1")?
        .query_row(params![key], read_prompt_row)
        .optional()
        .map_err(Into::into)
}

pub fn prompt_list() -> Result<Vec<PromptRow>> {
    let conn = open()?;
    let mut stmt = conn
        .prepare("SELECT key, value, default_hash, edited, updated_at FROM prompts ORDER BY key")?;
    let rows = stmt.query_map([], read_prompt_row)?;
    Ok(rows.flatten().collect())
}

/// Insert a shipped default. No-op when the key already exists.
pub fn prompt_seed(key: &str, value: &str, default_hash: &str) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "INSERT INTO prompts (key, value, default_hash, edited, updated_at) \
         VALUES (?1, ?2, ?3, 0, ?4) ON CONFLICT(key) DO NOTHING",
        params![
            key,
            value,
            default_hash,
            crate::message::epoch_secs() as i64
        ],
    )?;
    Ok(())
}

/// Adopt a new shipped default, but only for a row the user never edited.
pub fn prompt_refresh_default(key: &str, value: &str, default_hash: &str) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "UPDATE prompts SET value = ?2, default_hash = ?3, updated_at = ?4 \
         WHERE key = ?1 AND edited = 0",
        params![
            key,
            value,
            default_hash,
            crate::message::epoch_secs() as i64
        ],
    )?;
    Ok(())
}

/// Store a user edit. Marks the row so later releases leave it alone.
pub fn prompt_set(key: &str, value: &str, default_hash: &str) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "INSERT INTO prompts (key, value, default_hash, edited, updated_at) \
         VALUES (?1, ?2, ?3, 1, ?4) \
         ON CONFLICT(key) DO UPDATE SET value = ?2, edited = 1, updated_at = ?4",
        params![
            key,
            value,
            default_hash,
            crate::message::epoch_secs() as i64
        ],
    )?;
    Ok(())
}

/// Drop a row so the next sync reseeds it from the shipped default.
/// Returns true when a row was actually removed.
pub fn prompt_delete(key: &str) -> Result<bool> {
    let conn = open()?;
    let affected = conn.execute("DELETE FROM prompts WHERE key = ?1", params![key])?;
    Ok(affected > 0)
}
