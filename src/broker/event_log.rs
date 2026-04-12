use super::*;

/// One row from the `events` table.
#[derive(Debug, Clone)]
pub struct EventRow {
    pub id: i64,
    pub created_at: i64,
    pub level: String,
    pub source: String,
    pub message: String,
    pub details: Option<String>,
}

/// Append an event. Prefer [`try_log_event`] from call sites where failure must not propagate.
pub fn log_event(level: &str, source: &str, message: &str, details: Option<&str>) -> Result<()> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    conn.execute(
        "INSERT INTO events (created_at, level, source, message, details) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![now, level, source, message, details],
    )?;
    Ok(())
}

/// Best-effort persist. If the DB is unavailable, the event is silently dropped.
pub fn try_log_event(level: &str, source: &str, message: &str, details: Option<&str>) {
    let _ = log_event(level, source, message, details);
}

/// Convenience: log an error event.
pub fn try_log_error(source: &str, message: &str, details: Option<&str>) {
    try_log_event("error", source, message, details);
}

/// Recent events, newest first. Filter by level if provided.
pub fn events_recent(limit: usize, level: Option<&str>) -> Result<Vec<EventRow>> {
    let conn = open()?;
    let lim = limit.clamp(1, 500) as i64;
    let (sql, params_vec): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = match level {
        Some(lvl) => (
            "SELECT id, created_at, level, source, message, details FROM events WHERE level = ?1 ORDER BY id DESC LIMIT ?2",
            vec![Box::new(lvl.to_string()), Box::new(lim)],
        ),
        None => (
            "SELECT id, created_at, level, source, message, details FROM events ORDER BY id DESC LIMIT ?1",
            vec![Box::new(lim)],
        ),
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params_vec.iter()), |row| {
        Ok(EventRow {
            id: row.get(0)?,
            created_at: row.get(1)?,
            level: row.get(2)?,
            source: row.get(3)?,
            message: row.get(4)?,
            details: row.get(5)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// Delete all events, or only those matching a level.
pub fn events_clear(level: Option<&str>) -> Result<u64> {
    let conn = open()?;
    let changed = match level {
        Some(lvl) => conn.execute("DELETE FROM events WHERE level = ?1", params![lvl])?,
        None => conn.execute("DELETE FROM events", [])?,
    };
    Ok(changed as u64)
}
