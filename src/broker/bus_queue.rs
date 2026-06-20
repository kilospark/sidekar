use super::*;

/// A message waiting in the bus queue for delivery.
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    pub id: i64,
    pub sender: String,
    pub recipient: String,
    pub body: String,
    pub created_at: u64,
    /// When true, deliver by writing into the agent input and submitting (Enter).
    /// When false, print via side channel (stdout/tunnel) without touching agent input.
    pub submit_input: bool,
    pub envelope: Option<Envelope>,
}

/// Enqueue a message for delivery to `recipient`.
pub fn enqueue_message(sender: &str, recipient: &str, body: &str) -> Result<()> {
    enqueue_bus_message(recipient, sender, body, false, None)
}

/// Enqueue with explicit delivery mode and optional typed envelope metadata.
pub fn enqueue_bus_message(
    recipient: &str,
    sender: &str,
    body: &str,
    submit_input: bool,
    envelope: Option<&Envelope>,
) -> Result<()> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    let envelope_json = envelope
        .map(serde_json::to_string)
        .transpose()
        .context("serialize bus_queue envelope")?;
    conn.execute(
        "INSERT INTO bus_queue (recipient, sender, body, created_at, submit_input, envelope_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            recipient,
            sender,
            body,
            now,
            submit_input as i64,
            envelope_json,
        ],
    )?;
    Ok(())
}

/// Check if there are pending bus messages without consuming them.
///
/// Called from the REPL input loop on every poll-timeout tick, so it must
/// stay cheap — uses a thread-local cached connection to avoid the open() +
/// schema-check syscalls on every keystroke.
pub fn has_pending_messages(recipient: &str) -> bool {
    with_cached_conn(|conn| {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM bus_queue WHERE recipient = ?1)",
            params![recipient],
            |row| row.get::<_, bool>(0),
        )
    })
    .unwrap_or(false)
}

fn map_queued_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueuedMessage> {
    let envelope_json: Option<String> = row.get(5)?;
    let envelope = envelope_json
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok());
    Ok(QueuedMessage {
        id: row.get(0)?,
        sender: row.get(1)?,
        recipient: row.get(2)?,
        body: row.get(3)?,
        created_at: row.get::<_, i64>(4)? as u64,
        submit_input: row.get::<_, i64>(6).unwrap_or(0) != 0,
        envelope,
    })
}

/// Poll for messages addressed to `recipient`. Returns all pending messages
/// and deletes them from the queue (atomic read-and-delete).
pub fn poll_messages(recipient: &str) -> Result<Vec<QueuedMessage>> {
    let conn = open()?;
    let tx = conn.unchecked_transaction()?;

    let messages: Vec<QueuedMessage> = {
        let mut stmt = tx.prepare(
            "SELECT id, sender, recipient, body, created_at, envelope_json, submit_input
             FROM bus_queue WHERE recipient = ?1 ORDER BY id",
        )?;
        stmt.query_map(params![recipient], map_queued_row)?
            .filter_map(|r| r.ok())
            .collect()
    };

    if !messages.is_empty() {
        tx.execute(
            "DELETE FROM bus_queue WHERE recipient = ?1",
            params![recipient],
        )?;
    }

    tx.commit()?;
    Ok(messages)
}

/// Clean up old messages (safety net for undelivered messages from dead agents).
pub fn cleanup_old_messages(max_age_secs: u64) -> Result<usize> {
    let conn = open()?;
    let cutoff = (crate::message::epoch_secs() - max_age_secs) as i64;
    let deleted = conn.execute(
        "DELETE FROM bus_queue WHERE created_at < ?1",
        params![cutoff],
    )?;
    Ok(deleted)
}

/// Remove queued nudge rows for a request that is no longer open.
pub fn purge_nudges_for_request(msg_id: &str) -> Result<usize> {
    let conn = open()?;
    let pattern = format!("%--reply-to={msg_id}%");
    let deleted = conn.execute(
        "DELETE FROM bus_queue WHERE instr(body, '[sidekar]') = 1 AND body LIKE ?1",
        params![pattern],
    )?;
    Ok(deleted)
}
