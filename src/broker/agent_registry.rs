use super::*;

#[derive(Debug, Clone)]
pub struct BrokerAgent {
    pub id: AgentId,
    pub pane_unique_id: Option<String>,
    pub socket_path: Option<String>,
    pub cwd: Option<String>,
    pub registered_at: u64,
    pub last_seen_at: u64,
}

pub fn register_agent(agent: &AgentId, pane_unique_id: Option<&str>) -> Result<()> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    let pane = agent.pane.as_deref();
    let session = agent.session.as_deref();
    let nick = agent.nick.as_deref();
    let agent_type = agent.agent_type.as_deref();
    let pane_unique_id = pane_unique_id.map(str::to_string);
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM agents WHERE name = ?1", params![agent.name])?;
    if let Some(ref unique) = pane_unique_id {
        tx.execute(
            "DELETE FROM agents WHERE pane_unique_id = ?1",
            params![unique],
        )?;
    }
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().to_string());
    tx.execute(
        "INSERT INTO agents (
            name, nick, session, pane, pane_unique_id, agent_type, socket_path, cwd, registered_at, last_seen_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?8)",
        params![
            agent.name,
            nick,
            session,
            pane,
            pane_unique_id,
            agent_type,
            cwd,
            now,
        ],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn set_agent_socket_path(name: &str, socket_path: Option<&Path>) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "UPDATE agents SET socket_path = ?2, last_seen_at = ?3 WHERE name = ?1",
        params![
            name,
            socket_path.map(|p| p.to_string_lossy().to_string()),
            crate::message::epoch_secs() as i64
        ],
    )?;
    Ok(())
}

pub fn touch_agent(name: &str) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "UPDATE agents SET last_seen_at = ?2 WHERE name = ?1",
        params![name, crate::message::epoch_secs() as i64],
    )?;
    Ok(())
}

pub fn unregister_agent(name: &str) -> Result<()> {
    let conn = open()?;
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM agents WHERE name = ?1", params![name])?;
    tx.execute(
        "DELETE FROM pending_requests WHERE recipient_name = ?1",
        params![name],
    )?;
    tx.execute(
        "DELETE FROM outbound_requests WHERE sender_name = ?1",
        params![name],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn agent_for_pane_unique(pane_unique_id: &str) -> Result<Option<BrokerAgent>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT name, nick, session, pane, pane_unique_id, agent_type, socket_path, cwd, registered_at, last_seen_at
         FROM agents
         WHERE pane_unique_id = ?1
         LIMIT 1",
    )?;
    stmt.query_row(params![pane_unique_id], row_to_agent)
        .optional()
        .map_err(Into::into)
}

pub fn list_agents(session: Option<&str>) -> Result<Vec<BrokerAgent>> {
    let conn = open()?;
    let sql = if session.is_some() {
        "SELECT name, nick, session, pane, pane_unique_id, agent_type, socket_path, cwd, registered_at, last_seen_at
         FROM agents
         WHERE session = ?1
         ORDER BY name"
    } else {
        "SELECT name, nick, session, pane, pane_unique_id, agent_type, socket_path, cwd, registered_at, last_seen_at
         FROM agents
         ORDER BY name"
    };
    let mut stmt = conn.prepare(sql)?;
    let mut rows = if let Some(session) = session {
        stmt.query(params![session])?
    } else {
        stmt.query([])?
    };
    let mut agents = Vec::new();
    while let Some(row) = rows.next()? {
        agents.push(row_to_agent(row)?);
    }
    Ok(agents)
}

pub fn find_agent(target: &str, session: Option<&str>) -> Result<Option<BrokerAgent>> {
    let conn = open()?;
    let mut stmt = if session.is_some() {
        conn.prepare(
            "SELECT name, nick, session, pane, pane_unique_id, agent_type, socket_path, cwd, registered_at, last_seen_at
             FROM agents
             WHERE session = ?1 AND (name = ?2 OR nick = ?2)
             ORDER BY CASE WHEN name = ?2 THEN 0 ELSE 1 END
             LIMIT 1",
        )?
    } else {
        conn.prepare(
            "SELECT name, nick, session, pane, pane_unique_id, agent_type, socket_path, cwd, registered_at, last_seen_at
             FROM agents
             WHERE name = ?1 OR nick = ?1
             ORDER BY CASE WHEN name = ?1 THEN 0 ELSE 1 END
             LIMIT 1",
        )?
    };
    if let Some(session) = session {
        stmt.query_row(params![session, target], row_to_agent)
            .optional()
            .map_err(Into::into)
    } else {
        stmt.query_row(params![target], row_to_agent)
            .optional()
            .map_err(Into::into)
    }
}

fn row_to_agent(row: &rusqlite::Row<'_>) -> rusqlite::Result<BrokerAgent> {
    Ok(BrokerAgent {
        id: AgentId {
            name: row.get(0)?,
            nick: row.get(1)?,
            session: row.get(2)?,
            pane: row.get(3)?,
            agent_type: row.get(5)?,
        },
        pane_unique_id: row.get(4)?,
        socket_path: row.get(6)?,
        cwd: row.get(7)?,
        registered_at: row.get::<_, i64>(8)? as u64,
        last_seen_at: row.get::<_, i64>(9)? as u64,
    })
}
