use super::*;

pub struct ProxyLogEntry {
    pub method: String,
    pub path: String,
    pub upstream_host: String,
    pub request_headers: String,
    pub request_body: Vec<u8>,
    pub response_status: u16,
    pub response_headers: String,
    pub response_body: Vec<u8>,
    pub duration_ms: u64,
}

pub fn proxy_log_insert(entry: &ProxyLogEntry) -> Result<()> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    conn.execute(
        "INSERT INTO proxy_log (created_at, method, path, upstream_host, request_headers, request_body, response_status, response_headers, response_body, duration_ms, compressed)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,0)",
        params![
            now,
            entry.method,
            entry.path,
            entry.upstream_host,
            entry.request_headers,
            entry.request_body,
            entry.response_status as i64,
            entry.response_headers,
            entry.response_body,
            entry.duration_ms as i64,
        ],
    )?;
    Ok(())
}

pub struct ProxyLogRow {
    pub id: i64,
    pub created_at: i64,
    pub method: String,
    pub path: String,
    pub upstream_host: String,
    pub request_headers: String,
    pub request_body: Vec<u8>,
    pub response_status: i64,
    pub response_headers: String,
    pub response_body: Vec<u8>,
    pub duration_ms: i64,
}

pub fn proxy_log_recent(limit: usize) -> Result<Vec<ProxyLogRow>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT id, created_at, method, path, upstream_host, request_headers, request_body, response_status, response_headers, response_body, duration_ms
         FROM proxy_log ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok(ProxyLogRow {
            id: row.get(0)?,
            created_at: row.get(1)?,
            method: row.get(2)?,
            path: row.get(3)?,
            upstream_host: row.get(4)?,
            request_headers: row.get(5)?,
            request_body: row.get(6)?,
            response_status: row.get(7)?,
            response_headers: row.get(8)?,
            response_body: row.get(9)?,
            duration_ms: row.get(10)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn proxy_log_detail(id: i64) -> Result<Option<ProxyLogRow>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT id, created_at, method, path, upstream_host, request_headers, request_body, response_status, response_headers, response_body, duration_ms
         FROM proxy_log WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![id], |row| {
        Ok(ProxyLogRow {
            id: row.get(0)?,
            created_at: row.get(1)?,
            method: row.get(2)?,
            path: row.get(3)?,
            upstream_host: row.get(4)?,
            request_headers: row.get(5)?,
            request_body: row.get(6)?,
            response_status: row.get(7)?,
            response_headers: row.get(8)?,
            response_body: row.get(9)?,
            duration_ms: row.get(10)?,
        })
    })?;
    match rows.next() {
        Some(Ok(row)) => Ok(Some(row)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

pub fn proxy_log_clear() -> Result<u64> {
    let conn = open()?;
    let count = conn.execute("DELETE FROM proxy_log", [])?;
    Ok(count as u64)
}

pub fn proxy_log_prune(max_age_secs: i64) -> Result<u64> {
    let conn = open()?;
    let cutoff = crate::message::epoch_secs() as i64 - max_age_secs;
    let count = conn.execute(
        "DELETE FROM proxy_log WHERE created_at < ?1",
        params![cutoff],
    )?;
    Ok(count as u64)
}
