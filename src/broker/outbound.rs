use super::*;

pub(crate) const OUTBOUND_STATUS_OPEN: &str = "open";
pub(crate) const OUTBOUND_STATUS_ANSWERED: &str = "answered";
pub(crate) const OUTBOUND_STATUS_TIMED_OUT: &str = "timed_out";
pub(crate) const OUTBOUND_STATUS_CANCELLED: &str = "cancelled";

#[derive(Debug, Clone)]
pub struct OutboundRequestRecord {
    pub msg_id: String,
    pub sender_name: String,
    pub sender_label: String,
    pub recipient_name: String,
    pub transport_name: String,
    pub transport_target: String,
    pub kind: String,
    pub channel: Option<String>,
    pub project: Option<String>,
    pub message_preview: String,
    pub status: String,
    pub created_at: u64,
    pub nudge_count: u32,
    pub last_nudged_at: Option<u64>,
    pub answered_at: Option<u64>,
    pub timed_out_at: Option<u64>,
    pub closed_at: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct BusReplyRecord {
    pub reply_to_msg_id: String,
    pub reply_msg_id: String,
    pub sender_name: String,
    pub sender_label: String,
    pub kind: String,
    pub message: String,
    pub created_at: u64,
    pub envelope_json: String,
}

pub fn set_pending(envelope: &Envelope) -> Result<()> {
    let conn = open()?;
    let envelope_json = serde_json::to_string(envelope)?;
    conn.execute(
        "INSERT OR REPLACE INTO pending_requests (id, recipient_name, envelope_json, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            envelope.id,
            envelope.to,
            envelope_json,
            envelope.created_at as i64
        ],
    )?;
    Ok(())
}

pub fn clear_pending(msg_id: &str) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "DELETE FROM pending_requests WHERE id = ?1",
        params![msg_id],
    )?;
    Ok(())
}

pub fn clear_pending_for_agent(name: &str) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "DELETE FROM pending_requests WHERE recipient_name = ?1",
        params![name],
    )?;
    Ok(())
}

pub fn pending_for_agent(name: &str) -> Result<Vec<Envelope>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT envelope_json
         FROM pending_requests
         WHERE recipient_name = ?1
         ORDER BY created_at ASC",
    )?;
    let mut rows = stmt.query(params![name])?;
    let mut envelopes = Vec::new();
    while let Some(row) = rows.next()? {
        let envelope_json: String = row.get(0)?;
        let envelope = serde_json::from_str::<Envelope>(&envelope_json)?;
        envelopes.push(envelope);
    }
    Ok(envelopes)
}

pub fn pending_message(msg_id: &str) -> Result<Option<Envelope>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT envelope_json
         FROM pending_requests
         WHERE id = ?1
         LIMIT 1",
    )?;
    stmt.query_row(params![msg_id], |row| row.get::<_, String>(0))
        .optional()?
        .map(|envelope_json| serde_json::from_str::<Envelope>(&envelope_json))
        .transpose()
        .map_err(Into::into)
}

pub fn set_outbound_request(
    envelope: &Envelope,
    sender_label: &str,
    transport_name: &str,
    transport_target: &str,
    channel: Option<&str>,
    project: Option<&str>,
) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "INSERT OR REPLACE INTO outbound_requests (
            msg_id, sender_name, sender_label, recipient_name, transport_name, transport_target,
            kind, channel, project, message_preview, status, created_at, nudge_count,
            last_nudged_at, answered_at, timed_out_at, closed_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, NULL, NULL, NULL, NULL)",
        params![
            envelope.id,
            envelope.from.name,
            sender_label,
            envelope.to,
            transport_name,
            transport_target,
            envelope.kind.as_str(),
            channel,
            project,
            envelope.preview(),
            OUTBOUND_STATUS_OPEN,
            envelope.created_at as i64,
        ],
    )?;
    Ok(())
}

pub fn delete_outbound_request(msg_id: &str) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "DELETE FROM outbound_requests WHERE msg_id = ?1",
        params![msg_id],
    )?;
    Ok(())
}

pub fn delete_outbound_for_sender(name: &str) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "DELETE FROM outbound_requests WHERE sender_name = ?1",
        params![name],
    )?;
    Ok(())
}

pub fn resolve_reply(msg_id: &str) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "DELETE FROM pending_requests WHERE id = ?1",
        params![msg_id],
    )?;
    Ok(())
}

pub fn record_reply(reply_to_msg_id: &str, envelope: &Envelope) -> Result<()> {
    let conn = open()?;
    let tx = conn.unchecked_transaction()?;
    let envelope_json =
        serde_json::to_string(envelope).context("failed to serialize reply envelope")?;
    tx.execute(
        "INSERT INTO bus_replies (
            reply_to_msg_id, reply_msg_id, sender_name, sender_label, kind, message, created_at, envelope_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            reply_to_msg_id,
            envelope.id,
            envelope.from.name,
            envelope.from.display_name(),
            envelope.kind.as_str(),
            envelope.message,
            envelope.created_at as i64,
            envelope_json,
        ],
    )?;
    tx.execute(
        "DELETE FROM pending_requests WHERE id = ?1",
        params![reply_to_msg_id],
    )?;
    tx.execute(
        "UPDATE outbound_requests
         SET status = ?2,
             answered_at = COALESCE(answered_at, ?3),
             closed_at = COALESCE(closed_at, ?3)
         WHERE msg_id = ?1",
        params![
            reply_to_msg_id,
            OUTBOUND_STATUS_ANSWERED,
            envelope.created_at as i64
        ],
    )?;
    tx.execute(
        "UPDATE agent_sessions
         SET reply_count = reply_count + 1,
             message_count = message_count + 1,
             last_reply_msg_id = ?2,
             last_active_at = ?3
         WHERE id = (
             SELECT id
             FROM agent_sessions
             WHERE agent_name = (
                 SELECT sender_name FROM outbound_requests WHERE msg_id = ?1
             )
               AND ended_at IS NULL
             ORDER BY started_at DESC
             LIMIT 1
         )",
        params![reply_to_msg_id, envelope.id, envelope.created_at as i64],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn outbound_request(msg_id: &str) -> Result<Option<OutboundRequestRecord>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT msg_id, sender_name, sender_label, recipient_name, transport_name, transport_target,
                kind, channel, project, message_preview, status, created_at, nudge_count,
                last_nudged_at, answered_at, timed_out_at, closed_at
         FROM outbound_requests
         WHERE msg_id = ?1
         LIMIT 1",
    )?;
    stmt.query_row(params![msg_id], row_to_outbound)
        .optional()
        .map_err(Into::into)
}

pub fn outbound_for_sender(name: &str) -> Result<Vec<OutboundRequestRecord>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT msg_id, sender_name, sender_label, recipient_name, transport_name, transport_target,
                kind, channel, project, message_preview, status, created_at, nudge_count,
                last_nudged_at, answered_at, timed_out_at, closed_at
         FROM outbound_requests
         WHERE sender_name = ?1 AND status = ?2
         ORDER BY created_at ASC",
    )?;
    let mut rows = stmt.query(params![name, OUTBOUND_STATUS_OPEN])?;
    let mut requests = Vec::new();
    while let Some(row) = rows.next()? {
        requests.push(row_to_outbound(row)?);
    }
    Ok(requests)
}

pub fn expired_outbound_for_sender(
    name: &str,
    created_at_cutoff: u64,
) -> Result<Vec<OutboundRequestRecord>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT msg_id, sender_name, sender_label, recipient_name, transport_name, transport_target,
                kind, channel, project, message_preview, status, created_at, nudge_count,
                last_nudged_at, answered_at, timed_out_at, closed_at
         FROM outbound_requests
         WHERE sender_name = ?1 AND status = ?2 AND created_at <= ?3
         ORDER BY created_at ASC",
    )?;
    let mut rows = stmt.query(params![
        name,
        OUTBOUND_STATUS_OPEN,
        created_at_cutoff as i64
    ])?;
    let mut requests = Vec::new();
    while let Some(row) = rows.next()? {
        requests.push(row_to_outbound(row)?);
    }
    Ok(requests)
}

pub fn increment_nudge_count(msg_id: &str, nudged_at: u64) -> Result<u32> {
    let conn = open()?;
    conn.execute(
        "UPDATE outbound_requests
         SET nudge_count = nudge_count + 1,
             last_nudged_at = ?2
         WHERE msg_id = ?1",
        params![msg_id, nudged_at as i64],
    )?;
    let count = conn.query_row(
        "SELECT nudge_count FROM outbound_requests WHERE msg_id = ?1",
        params![msg_id],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(count as u32)
}

pub fn mark_outbound_timed_out(msg_id: &str, timed_out_at: u64) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "UPDATE outbound_requests
         SET status = ?2,
             timed_out_at = COALESCE(timed_out_at, ?3),
             closed_at = COALESCE(closed_at, ?3)
         WHERE msg_id = ?1 AND status = ?4",
        params![
            msg_id,
            OUTBOUND_STATUS_TIMED_OUT,
            timed_out_at as i64,
            OUTBOUND_STATUS_OPEN
        ],
    )?;
    Ok(())
}

/// Mark a single outbound request as cancelled (only if currently open)
/// and remove its pending_requests row so the nudger stops. Returns the
/// number of outbound rows updated (0 or 1).
pub fn cancel_outbound_request(msg_id: &str, cancelled_at: u64) -> Result<usize> {
    let mut conn = open()?;
    let tx = conn.transaction()?;
    let updated = tx.execute(
        "UPDATE outbound_requests
         SET status = ?2,
             closed_at = COALESCE(closed_at, ?3)
         WHERE msg_id = ?1 AND status = ?4",
        params![
            msg_id,
            OUTBOUND_STATUS_CANCELLED,
            cancelled_at as i64,
            OUTBOUND_STATUS_OPEN,
        ],
    )?;
    tx.execute(
        "DELETE FROM pending_requests WHERE id = ?1",
        params![msg_id],
    )?;
    tx.commit()?;
    Ok(updated)
}

/// Cancel all open outbound requests owned by `sender_name`. Returns the
/// list of msg_ids that were actually cancelled (were open → cancelled).
/// Pending rows for those msg_ids are also removed.
pub fn cancel_all_outbound_for_sender(
    sender_name: &str,
    cancelled_at: u64,
) -> Result<Vec<String>> {
    let mut conn = open()?;
    let tx = conn.transaction()?;

    let ids: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT msg_id
             FROM outbound_requests
             WHERE sender_name = ?1 AND status = ?2",
        )?;
        let rows = stmt.query_map(params![sender_name, OUTBOUND_STATUS_OPEN], |r| {
            r.get::<_, String>(0)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    if ids.is_empty() {
        tx.commit()?;
        return Ok(ids);
    }

    tx.execute(
        "UPDATE outbound_requests
         SET status = ?2,
             closed_at = COALESCE(closed_at, ?3)
         WHERE sender_name = ?1 AND status = ?4",
        params![
            sender_name,
            OUTBOUND_STATUS_CANCELLED,
            cancelled_at as i64,
            OUTBOUND_STATUS_OPEN,
        ],
    )?;
    for id in &ids {
        tx.execute(
            "DELETE FROM pending_requests WHERE id = ?1",
            params![id],
        )?;
    }
    tx.commit()?;
    Ok(ids)
}

pub fn list_outbound_requests_for_sender(
    sender_name: &str,
    status: Option<&str>,
    limit: usize,
) -> Result<Vec<OutboundRequestRecord>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT msg_id, sender_name, sender_label, recipient_name, transport_name, transport_target,
                kind, channel, project, message_preview, status, created_at, nudge_count,
                last_nudged_at, answered_at, timed_out_at, closed_at
         FROM outbound_requests
         WHERE sender_name = ?1 AND (?2 IS NULL OR status = ?2)
         ORDER BY created_at DESC
         LIMIT ?3",
    )?;
    let mut rows = stmt.query(params![sender_name, status, limit.max(1) as i64])?;
    let mut requests = Vec::new();
    while let Some(row) = rows.next()? {
        requests.push(row_to_outbound(row)?);
    }
    Ok(requests)
}

pub fn list_bus_replies_for_sender(
    sender_name: &str,
    reply_to_msg_id: Option<&str>,
    limit: usize,
) -> Result<Vec<BusReplyRecord>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT r.reply_to_msg_id, r.reply_msg_id, r.sender_name, r.sender_label, r.kind,
                r.message, r.created_at, r.envelope_json
         FROM bus_replies r
         INNER JOIN outbound_requests o ON o.msg_id = r.reply_to_msg_id
         WHERE o.sender_name = ?1 AND (?2 IS NULL OR r.reply_to_msg_id = ?2)
         ORDER BY r.created_at DESC
         LIMIT ?3",
    )?;
    let mut rows = stmt.query(params![sender_name, reply_to_msg_id, limit.max(1) as i64])?;
    let mut replies = Vec::new();
    while let Some(row) = rows.next()? {
        replies.push(row_to_bus_reply(row)?);
    }
    Ok(replies)
}

pub fn replies_for_request(msg_id: &str) -> Result<Vec<BusReplyRecord>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT reply_to_msg_id, reply_msg_id, sender_name, sender_label, kind, message, created_at, envelope_json
         FROM bus_replies
         WHERE reply_to_msg_id = ?1
         ORDER BY created_at ASC",
    )?;
    let mut rows = stmt.query(params![msg_id])?;
    let mut replies = Vec::new();
    while let Some(row) = rows.next()? {
        replies.push(row_to_bus_reply(row)?);
    }
    Ok(replies)
}

pub fn clear_pending_between_agents(recipient_name: &str, sender_name: &str) -> Result<usize> {
    let conn = open()?;
    let pending = pending_for_agent(recipient_name)?;
    let mut deleted = 0usize;
    for env in pending {
        if env.from.name == sender_name {
            deleted += conn.execute(
                "DELETE FROM pending_requests WHERE id = ?1",
                params![env.id],
            )?;
        }
    }
    Ok(deleted)
}

pub fn close_outbound_between_agents(
    sender_name: &str,
    recipient_name: &str,
    keep_msg_id: Option<&str>,
) -> Result<usize> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    let updated = if let Some(keep_msg_id) = keep_msg_id {
        conn.execute(
            "UPDATE outbound_requests
             SET status = ?4,
                 closed_at = COALESCE(closed_at, ?5)
             WHERE sender_name = ?1 AND recipient_name = ?2 AND msg_id != ?3 AND status = ?6",
            params![
                sender_name,
                recipient_name,
                keep_msg_id,
                OUTBOUND_STATUS_CANCELLED,
                now,
                OUTBOUND_STATUS_OPEN
            ],
        )?
    } else {
        conn.execute(
            "UPDATE outbound_requests
             SET status = ?3,
                 closed_at = COALESCE(closed_at, ?4)
             WHERE sender_name = ?1 AND recipient_name = ?2 AND status = ?5",
            params![
                sender_name,
                recipient_name,
                OUTBOUND_STATUS_CANCELLED,
                now,
                OUTBOUND_STATUS_OPEN
            ],
        )?
    };
    Ok(updated)
}

pub fn cleanup_old_pending_requests(max_age_secs: u64) -> Result<usize> {
    let conn = open()?;
    let cutoff = (crate::message::epoch_secs() - max_age_secs) as i64;
    let deleted = conn.execute(
        "DELETE FROM pending_requests WHERE created_at < ?1",
        params![cutoff],
    )?;
    Ok(deleted)
}

pub fn cleanup_old_outbound_requests(max_age_secs: u64) -> Result<usize> {
    let conn = open()?;
    let cutoff = (crate::message::epoch_secs() - max_age_secs) as i64;
    let deleted = conn.execute(
        "DELETE FROM outbound_requests WHERE created_at < ?1",
        params![cutoff],
    )?;
    Ok(deleted)
}

fn row_to_outbound(row: &rusqlite::Row<'_>) -> rusqlite::Result<OutboundRequestRecord> {
    Ok(OutboundRequestRecord {
        msg_id: row.get(0)?,
        sender_name: row.get(1)?,
        sender_label: row.get(2)?,
        recipient_name: row.get(3)?,
        transport_name: row.get(4)?,
        transport_target: row.get(5)?,
        kind: row.get(6)?,
        channel: row.get(7)?,
        project: row.get(8)?,
        message_preview: row.get(9)?,
        status: row.get(10)?,
        created_at: row.get::<_, i64>(11)? as u64,
        nudge_count: row.get::<_, i64>(12)? as u32,
        last_nudged_at: row.get::<_, Option<i64>>(13)?.map(|v| v as u64),
        answered_at: row.get::<_, Option<i64>>(14)?.map(|v| v as u64),
        timed_out_at: row.get::<_, Option<i64>>(15)?.map(|v| v as u64),
        closed_at: row.get::<_, Option<i64>>(16)?.map(|v| v as u64),
    })
}

fn row_to_bus_reply(row: &rusqlite::Row<'_>) -> rusqlite::Result<BusReplyRecord> {
    Ok(BusReplyRecord {
        reply_to_msg_id: row.get(0)?,
        reply_msg_id: row.get(1)?,
        sender_name: row.get(2)?,
        sender_label: row.get(3)?,
        kind: row.get(4)?,
        message: row.get(5)?,
        created_at: row.get::<_, i64>(6)? as u64,
        envelope_json: row.get(7)?,
    })
}
