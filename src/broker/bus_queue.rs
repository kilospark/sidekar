use super::*;

/// A message waiting in the bus queue for delivery.
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    pub id: i64,
    pub sender: String,
    pub recipient: String,
    pub body: String,
    pub created_at: u64,
    /// Legacy delivery flag. PTY delivery treats queued bus rows as agent input.
    pub submit_input: bool,
    pub envelope: Option<Envelope>,
}

/// Enqueue a message for delivery to `recipient`.
pub fn enqueue_message(sender: &str, recipient: &str, body: &str) -> Result<()> {
    enqueue_bus_message(recipient, sender, body, true, None)
}

/// Enqueue with explicit delivery mode and optional typed envelope metadata.
pub fn enqueue_bus_message(
    recipient: &str,
    sender: &str,
    body: &str,
    _submit_input: bool,
    envelope: Option<&Envelope>,
) -> Result<()> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    let envelope_json = envelope
        .map(serde_json::to_string)
        .transpose()
        .context("serialize bus_queue envelope")?;
    conn.execute(
        "INSERT INTO bus_queue (recipient, sender, body, created_at, submit_input, envelope_json, envelope_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            recipient,
            sender,
            body,
            now,
            true,
            envelope_json,
            envelope.map(|e| e.id.as_str()),
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
            "SELECT EXISTS(
                SELECT 1 FROM bus_queue
                WHERE recipient = ?1 AND claimed_at = 0 AND delivered_at = 0
            )",
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

/// List pending messages without removing them (PTY poller peeks then acks per row).
pub fn list_queued_messages(recipient: &str) -> Result<Vec<QueuedMessage>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT id, sender, recipient, body, created_at, envelope_json, submit_input
         FROM bus_queue
         WHERE recipient = ?1 AND claimed_at = 0 AND delivered_at = 0
         ORDER BY id",
    )?;
    let messages = stmt
        .query_map(params![recipient], map_queued_row)?
        .filter_map(|r| r.ok())
        .collect();
    Ok(messages)
}

/// Atomically claim one queued message for delivery.
pub fn claim_queued_message(id: i64, recipient: &str) -> Result<Option<QueuedMessage>> {
    let conn = open()?;
    let tx = conn.unchecked_transaction()?;
    let message = {
        let mut stmt = tx.prepare(
            "SELECT id, sender, recipient, body, created_at, envelope_json, submit_input
             FROM bus_queue
             WHERE id = ?1 AND recipient = ?2 AND claimed_at = 0 AND delivered_at = 0",
        )?;
        stmt.query_row(params![id, recipient], map_queued_row)
            .optional()?
    };

    if message.is_some() {
        tx.execute(
            "UPDATE bus_queue SET claimed_at = ?1 WHERE id = ?2 AND claimed_at = 0",
            params![crate::message::epoch_secs() as i64, id],
        )?;
    }
    tx.commit()?;
    Ok(message)
}

/// Return a claimed message to the pending queue after a failed delivery attempt.
pub fn release_queued_message(id: i64) -> Result<()> {
    let conn = open()?;
    // `delivered_at != 0` means a poller already pasted this into its pane. Its
    // ack may have raced a claim release; resurrecting the row here would inject
    // the same message a second time.
    conn.execute(
        "UPDATE bus_queue SET claimed_at = 0 WHERE id = ?1 AND delivered_at = 0",
        params![id],
    )?;
    Ok(())
}

/// Record that `id` was pasted into the recipient's pane.
///
/// Idempotent and independent of who currently holds the claim, so a late ack
/// from a poller whose claim was released still closes the message out.
pub fn mark_message_delivered(id: i64) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "UPDATE bus_queue SET delivered_at = ?1 WHERE id = ?2 AND delivered_at = 0",
        params![crate::message::epoch_secs() as i64, id],
    )?;
    Ok(())
}

/// True while `envelope_id` is still sitting in the queue undelivered.
///
/// The nudge loop asks this so it cannot badger a recipient about a message
/// that has not reached their pane yet.
pub fn envelope_awaiting_delivery(envelope_id: &str) -> Result<bool> {
    let conn = open()?;
    let waiting: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM bus_queue WHERE envelope_id = ?1 AND delivered_at = 0
        )",
        params![envelope_id],
        |r| r.get(0),
    )?;
    Ok(waiting)
}

/// Return all claimed messages to the pending queue.
///
/// Called when the daemon starts, before it accepts bus clients. Claimed rows
/// from a dead daemon have no live owner and would otherwise be hidden forever.
pub fn release_all_claimed_messages() -> Result<usize> {
    let conn = open()?;
    let released = conn.execute(
        "UPDATE bus_queue SET claimed_at = 0 WHERE claimed_at != 0 AND delivered_at = 0",
        [],
    )?;
    Ok(released)
}

/// Remove one delivered message from the queue.
pub fn delete_queued_message(id: i64) -> Result<()> {
    let conn = open()?;
    conn.execute("DELETE FROM bus_queue WHERE id = ?1", params![id])?;
    Ok(())
}

/// Poll for messages addressed to `recipient`. Returns all pending messages
/// and deletes them from the queue (atomic read-and-delete).
pub fn poll_messages(recipient: &str) -> Result<Vec<QueuedMessage>> {
    let conn = open()?;
    let tx = conn.unchecked_transaction()?;
    let messages: Vec<QueuedMessage> = {
        let mut stmt = tx.prepare(
            "SELECT id, sender, recipient, body, created_at, envelope_json, submit_input
             FROM bus_queue
             WHERE recipient = ?1 AND claimed_at = 0 AND delivered_at = 0
             ORDER BY id",
        )?;
        stmt.query_map(params![recipient], map_queued_row)?
            .filter_map(|r| r.ok())
            .collect()
    };

    let ids = messages.iter().map(|m| m.id).collect::<Vec<_>>();
    if !ids.is_empty() {
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!("DELETE FROM bus_queue WHERE id IN ({placeholders})");
        tx.execute(&sql, rusqlite::params_from_iter(ids))?;
    }
    tx.commit()?;
    Ok(messages)
}

/// Clean up old messages (safety net for undelivered messages from dead agents).
///
/// A claimed row that has not been delivered is owned by a live poller which may
/// still be waiting for its pane's input gate to open. Deleting it there loses
/// the message with no record at either end, so age alone is not enough to reap.
/// Claims held by pollers that died are returned to `claimed_at = 0` by
/// `release_all_claimed_messages` at daemon start and by `release_agent_claims`
/// on detach, so nothing becomes permanently unreapable.
pub fn cleanup_old_messages(max_age_secs: u64) -> Result<usize> {
    let conn = open()?;
    let cutoff = (crate::message::epoch_secs() - max_age_secs) as i64;
    let deleted = conn.execute(
        "DELETE FROM bus_queue
         WHERE created_at < ?1 AND (claimed_at = 0 OR delivered_at != 0)",
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

/// Remove every generated nudge row from the bus queue.
pub fn purge_all_queued_nudges() -> Result<usize> {
    let conn = open()?;
    let deleted = conn.execute(
        "DELETE FROM bus_queue WHERE instr(body, '[sidekar] You have an unanswered request') = 1",
        [],
    )?;
    Ok(deleted)
}
