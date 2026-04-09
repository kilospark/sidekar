use super::*;

#[derive(Debug, Clone)]
pub struct AgentSessionRecord {
    pub id: String,
    pub agent_name: String,
    pub agent_type: Option<String>,
    pub display_name: Option<String>,
    pub nick: Option<String>,
    pub project: String,
    pub channel: Option<String>,
    pub cwd: Option<String>,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub last_active_at: u64,
    pub request_count: u64,
    pub reply_count: u64,
    pub message_count: u64,
    pub last_request_msg_id: Option<String>,
    pub last_reply_msg_id: Option<String>,
    pub notes: Option<String>,
}

pub fn create_agent_session(
    id: &str,
    agent_name: &str,
    agent_type: Option<&str>,
    nick: Option<&str>,
    project: &str,
    channel: Option<&str>,
    cwd: Option<&str>,
    started_at: u64,
) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "INSERT OR REPLACE INTO agent_sessions (
            id, agent_name, agent_type, display_name, nick, project, channel, cwd, started_at,
            ended_at, last_active_at, request_count, reply_count, message_count,
            last_request_msg_id, last_reply_msg_id, notes
        ) VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7, ?8, NULL, ?8, 0, 0, 0, NULL, NULL, NULL)",
        params![
            id,
            agent_name,
            agent_type,
            nick,
            project,
            channel,
            cwd,
            started_at as i64
        ],
    )?;
    Ok(())
}

pub fn finish_agent_session(id: &str, ended_at: u64) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "UPDATE agent_sessions
         SET ended_at = COALESCE(ended_at, ?2),
             last_active_at = CASE
                 WHEN last_active_at < ?2 THEN ?2
                 ELSE last_active_at
             END
         WHERE id = ?1",
        params![id, ended_at as i64],
    )?;
    Ok(())
}

pub fn mark_agent_session_request(agent_name: &str, msg_id: &str, created_at: u64) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "UPDATE agent_sessions
         SET request_count = request_count + 1,
             message_count = message_count + 1,
             last_request_msg_id = ?2,
             last_active_at = ?3
         WHERE id = (
             SELECT id
             FROM agent_sessions
             WHERE agent_name = ?1 AND ended_at IS NULL
             ORDER BY started_at DESC
             LIMIT 1
         )",
        params![agent_name, msg_id, created_at as i64],
    )?;
    Ok(())
}

pub fn list_agent_sessions(
    active_only: bool,
    project: Option<&str>,
    limit: usize,
) -> Result<Vec<AgentSessionRecord>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT id, agent_name, agent_type, display_name, nick, project, channel, cwd, started_at, ended_at,
                last_active_at, request_count, reply_count, message_count,
                last_request_msg_id, last_reply_msg_id, notes
         FROM agent_sessions
         WHERE (?1 = 0 OR ended_at IS NULL)
           AND (?2 IS NULL OR project = ?2)
         ORDER BY last_active_at DESC, started_at DESC
         LIMIT ?3",
    )?;
    let mut rows = stmt.query(params![
        if active_only { 1 } else { 0 },
        project,
        limit.max(1) as i64
    ])?;
    let mut sessions = Vec::new();
    while let Some(row) = rows.next()? {
        sessions.push(row_to_agent_session(row)?);
    }
    Ok(sessions)
}

pub fn get_agent_session(id: &str) -> Result<Option<AgentSessionRecord>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT id, agent_name, agent_type, display_name, nick, project, channel, cwd, started_at, ended_at,
                last_active_at, request_count, reply_count, message_count,
                last_request_msg_id, last_reply_msg_id, notes
         FROM agent_sessions
         WHERE id = ?1
         LIMIT 1",
    )?;
    stmt.query_row(params![id], row_to_agent_session)
        .optional()
        .map_err(Into::into)
}

pub fn set_agent_session_display_name(id: &str, display_name: Option<&str>) -> Result<bool> {
    let conn = open()?;
    let rows = conn.execute(
        "UPDATE agent_sessions
         SET display_name = ?2
         WHERE id = ?1",
        params![id, display_name],
    )?;
    Ok(rows > 0)
}

pub fn set_agent_session_notes(id: &str, notes: Option<&str>) -> Result<bool> {
    let conn = open()?;
    let rows = conn.execute(
        "UPDATE agent_sessions
         SET notes = ?2
         WHERE id = ?1",
        params![id, notes],
    )?;
    Ok(rows > 0)
}

fn row_to_agent_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentSessionRecord> {
    Ok(AgentSessionRecord {
        id: row.get(0)?,
        agent_name: row.get(1)?,
        agent_type: row.get(2)?,
        display_name: row.get(3)?,
        nick: row.get(4)?,
        project: row.get(5)?,
        channel: row.get(6)?,
        cwd: row.get(7)?,
        started_at: row.get::<_, i64>(8)? as u64,
        ended_at: row.get::<_, Option<i64>>(9)?.map(|v| v as u64),
        last_active_at: row.get::<_, i64>(10)? as u64,
        request_count: row.get::<_, i64>(11)? as u64,
        reply_count: row.get::<_, i64>(12)? as u64,
        message_count: row.get::<_, i64>(13)? as u64,
        last_request_msg_id: row.get(14)?,
        last_reply_msg_id: row.get(15)?,
        notes: row.get(16)?,
    })
}
