use super::*;

/// A message waiting in the bus queue for delivery.
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    pub id: i64,
    pub sender: String,
    pub recipient: String,
    pub body: String,
    pub created_at: u64,
}

/// Enqueue a message for delivery to `recipient`.
pub fn enqueue_message(sender: &str, recipient: &str, body: &str) -> Result<()> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    conn.execute(
        "INSERT INTO bus_queue (recipient, sender, body, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![recipient, sender, body, now],
    )?;
    Ok(())
}

/// Check if there are pending bus messages without consuming them.
pub fn has_pending_messages(recipient: &str) -> bool {
    let Ok(conn) = open() else { return false };
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bus_queue WHERE recipient = ?1)",
        params![recipient],
        |row| row.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

/// Poll for messages addressed to `recipient`. Returns all pending messages
/// and deletes them from the queue (atomic read-and-delete).
pub fn poll_messages(recipient: &str) -> Result<Vec<QueuedMessage>> {
    let conn = open()?;
    let tx = conn.unchecked_transaction()?;

    let messages: Vec<QueuedMessage> = {
        let mut stmt = tx.prepare(
            "SELECT id, sender, recipient, body, created_at FROM bus_queue WHERE recipient = ?1 ORDER BY id"
        )?;
        stmt.query_map(params![recipient], |row| {
            Ok(QueuedMessage {
                id: row.get(0)?,
                sender: row.get(1)?,
                recipient: row.get(2)?,
                body: row.get(3)?,
                created_at: row.get::<_, i64>(4)? as u64,
            })
        })?
        .filter_map(|r| r.ok())
        .collect()
    }; // stmt dropped here, releasing borrow on tx

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
