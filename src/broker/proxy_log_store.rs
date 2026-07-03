use super::*;

pub const PROXY_STATUS_PENDING: &str = "pending";
pub const PROXY_STATUS_COMPLETE: &str = "complete";
pub const PROXY_STATUS_FAILED: &str = "failed";

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

pub struct ProxyLogBegin {
    pub method: String,
    pub path: String,
    pub upstream_host: String,
    pub request_headers: String,
    pub request_body: Vec<u8>,
}

pub struct ProxyLogFinish {
    pub response_status: u16,
    pub response_headers: String,
    pub response_body: Vec<u8>,
    pub request_body: Vec<u8>,
    pub duration_ms: u64,
}

pub struct ProxyLogRow {
    pub id: i64,
    pub created_at: i64,
    pub status: String,
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

fn map_proxy_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProxyLogRow> {
    Ok(ProxyLogRow {
        id: row.get(0)?,
        created_at: row.get(1)?,
        status: row.get(2)?,
        method: row.get(3)?,
        path: row.get(4)?,
        upstream_host: row.get(5)?,
        request_headers: row.get(6)?,
        request_body: row.get(7)?,
        response_status: row.get(8)?,
        response_headers: row.get(9)?,
        response_body: row.get(10)?,
        duration_ms: row.get(11)?,
    })
}

const PROXY_ROW_SELECT: &str = "SELECT id, created_at, status, method, path, upstream_host, request_headers, request_body, response_status, response_headers, response_body, duration_ms FROM proxy_log";

pub fn proxy_log_insert(entry: &ProxyLogEntry) -> Result<()> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    conn.execute(
        "INSERT INTO proxy_log (created_at, status, method, path, upstream_host, request_headers, request_body, response_status, response_headers, response_body, duration_ms, compressed)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,0)",
        params![
            now,
            PROXY_STATUS_COMPLETE,
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

pub fn proxy_log_begin(entry: &ProxyLogBegin) -> Result<i64> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    conn.execute(
        "INSERT INTO proxy_log (created_at, status, method, path, upstream_host, request_headers, request_body, response_status, response_headers, response_body, duration_ms, compressed)
         VALUES (?1,?2,?3,?4,?5,?6,?7,0,'',?8,0,0)",
        params![
            now,
            PROXY_STATUS_PENDING,
            entry.method,
            entry.path,
            entry.upstream_host,
            entry.request_headers,
            entry.request_body,
            &[] as &[u8],
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn proxy_log_finish(id: i64, finish: &ProxyLogFinish) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "UPDATE proxy_log SET status = ?2, request_body = ?3, response_status = ?4, response_headers = ?5, response_body = ?6, duration_ms = ?7
         WHERE id = ?1",
        params![
            id,
            PROXY_STATUS_COMPLETE,
            finish.request_body,
            finish.response_status as i64,
            finish.response_headers,
            finish.response_body,
            finish.duration_ms as i64,
        ],
    )?;
    Ok(())
}

pub fn proxy_log_fail(id: i64) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "UPDATE proxy_log SET status = ?2 WHERE id = ?1 AND status = ?3",
        params![id, PROXY_STATUS_FAILED, PROXY_STATUS_PENDING],
    )?;
    Ok(())
}

pub fn proxy_log_fail_stale_pending(max_age_secs: i64) -> Result<u64> {
    let conn = open()?;
    let cutoff = crate::message::epoch_secs() as i64 - max_age_secs;
    let count = conn.execute(
        "UPDATE proxy_log SET status = ?1 WHERE status = ?2 AND created_at < ?3",
        params![PROXY_STATUS_FAILED, PROXY_STATUS_PENDING, cutoff],
    )?;
    Ok(count as u64)
}

pub fn proxy_log_recent(limit: usize) -> Result<Vec<ProxyLogRow>> {
    let conn = open()?;
    let mut stmt = conn.prepare(&format!("{PROXY_ROW_SELECT} ORDER BY id DESC LIMIT ?1"))?;
    let rows = stmt.query_map(params![limit as i64], map_proxy_row)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn proxy_log_since(since_id: i64, limit: usize) -> Result<Vec<ProxyLogRow>> {
    let conn = open()?;
    let lim = limit.clamp(1, 200) as i64;
    let mut stmt = conn.prepare(&format!(
        "{PROXY_ROW_SELECT} WHERE id > ?1 ORDER BY id ASC LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![since_id, lim], map_proxy_row)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn proxy_log_by_ids(ids: &[i64]) -> Result<Vec<ProxyLogRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let conn = open()?;
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!("{PROXY_ROW_SELECT} WHERE id IN ({placeholders}) ORDER BY id DESC");
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<Box<dyn rusqlite::types::ToSql>> = ids
        .iter()
        .map(|id| Box::new(*id) as Box<dyn rusqlite::types::ToSql>)
        .collect();
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(param_refs.as_slice(), map_proxy_row)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn proxy_log_page(limit: usize, offset: usize) -> Result<(i64, Vec<ProxyLogRow>)> {
    let conn = open()?;
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM proxy_log", [], |r| r.get(0))?;
    let lim = limit.clamp(1, 200) as i64;
    let off = offset as i64;
    let mut stmt = conn.prepare(&format!(
        "{PROXY_ROW_SELECT} ORDER BY id DESC LIMIT ?1 OFFSET ?2"
    ))?;
    let rows = stmt.query_map(params![lim, off], map_proxy_row)?;
    let page = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok((total, page))
}

pub fn proxy_log_max_id() -> Result<i64> {
    let conn = open()?;
    let max_id: Option<i64> = conn.query_row("SELECT MAX(id) FROM proxy_log", [], |r| r.get(0))?;
    Ok(max_id.unwrap_or(0))
}

pub fn proxy_log_detail(id: i64) -> Result<Option<ProxyLogRow>> {
    let conn = open()?;
    let mut stmt = conn.prepare(&format!("{PROXY_ROW_SELECT} WHERE id = ?1"))?;
    let mut rows = stmt.query_map(params![id], map_proxy_row)?;
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
