//! Resolve the mail of an agent that has left, so nobody else receives it.
//!
//! Mail is addressed by name and names are reused: the lowest free number goes
//! to the next agent. And when an agent unregisters its undelivered mail is kept
//! rather than dropped — deliberately, so an agent re-registering does not lose
//! its own queue. Put those together and an agent that exited with mail pending
//! left that mail waiting on its name, and the next agent to take the name
//! received it. Observed: three reminders about an unanswered request, written
//! after their recipient had gone, delivered eight seconds after an unrelated
//! agent registered under the reused name.
//!
//! So when an agent leaves for good, what was still addressed to it is settled
//! then and there: open requests to it are closed, its queued messages are
//! withdrawn, and each sender is told once. The name comes free with nothing
//! waiting on it.
//!
//! Called *before* the agent is unregistered, never after, so there is no moment
//! at which the name is free and mail is still queued for it.

use super::*;

/// Sidekar's own reminders about an unanswered request.
///
/// Derived from the request rather than sent by anyone, so the request's own
/// closing notice covers them; bouncing each would tell a sender the same thing
/// once per reminder.
const NUDGE_PREFIX: &str = "[sidekar] You have an unanswered request";

/// Longest excerpt of a withdrawn message quoted back to its sender.
const EXCERPT_CHARS: usize = 120;

/// What settling one departed agent's mail did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BounceReport {
    /// Open requests to the agent, now closed as `recipient_gone`.
    pub requests_closed: usize,
    /// Undelivered queue rows withdrawn, reminders included.
    pub withdrawn: usize,
    /// Notices queued to senders, at most one per sender.
    pub notified: usize,
}

/// True when an agent is registered under exactly this name.
///
/// Not `find_agent`, which also matches nicknames: a nick is not an address,
/// and a sweep asking "is anyone still at this name" must not be answered by an
/// agent that merely shares a nick with the one that left.
pub fn agent_is_registered(name: &str) -> Result<bool> {
    let conn = open()?;
    Ok(conn
        .query_row(
            "SELECT 1 FROM agents WHERE name = ?1 LIMIT 1",
            params![name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Settle everything still addressed to `agent_name`, which has left for good.
///
/// `nick` is only for the notices; the agent is identified by name. Safe to call
/// more than once — a second call finds nothing left to settle.
pub fn bounce_mail_for_departed(agent_name: &str, nick: &str, now: u64) -> Result<BounceReport> {
    let mut report = BounceReport::default();
    // sender -> lines to tell them
    let mut notices: std::collections::BTreeMap<String, Vec<String>> = Default::default();

    let closed_ids: std::collections::HashSet<String>;
    {
        let mut conn = open()?;
        let tx = conn.transaction()?;

        // Open requests to the departed agent: close them honestly, as
        // `recipient_gone` rather than `cancelled`, which means the sender
        // withdrew.
        let requests: Vec<(String, String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT msg_id, sender_name, COALESCE(message_preview, '')
                 FROM outbound_requests
                 WHERE status = ?1 AND transport_name = 'broker' AND transport_target = ?2",
            )?;
            stmt.query_map(params![OUTBOUND_STATUS_OPEN, agent_name], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        for (msg_id, sender, preview) in &requests {
            tx.execute(
                "UPDATE outbound_requests
                 SET status = ?2, closed_at = COALESCE(closed_at, ?3)
                 WHERE msg_id = ?1 AND status = ?4",
                params![
                    msg_id,
                    OUTBOUND_STATUS_RECIPIENT_GONE,
                    now as i64,
                    OUTBOUND_STATUS_OPEN
                ],
            )?;
            tx.execute(
                "DELETE FROM pending_requests WHERE id = ?1",
                params![msg_id],
            )?;
            notices
                .entry(sender.clone())
                .or_default()
                .push(unanswered_line(msg_id, preview));
        }
        report.requests_closed = requests.len();
        closed_ids = requests.into_iter().map(|(id, _, _)| id).collect();

        // Everything still undelivered to it.
        let queued: Vec<(String, String, Option<String>)> = {
            let mut stmt = tx.prepare(
                "SELECT COALESCE(sender, ''), body, envelope_id
                 FROM bus_queue WHERE recipient = ?1 AND delivered_at = 0",
            )?;
            stmt.query_map(params![agent_name], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        report.withdrawn = tx.execute(
            "DELETE FROM bus_queue WHERE recipient = ?1 AND delivered_at = 0",
            params![agent_name],
        )?;
        for (sender, body, envelope_id) in queued {
            // A reminder, or the message behind a request already accounted
            // for above: the sender has been told once, which is enough.
            if body.starts_with(NUDGE_PREFIX)
                || envelope_id
                    .as_deref()
                    .is_some_and(|id| closed_ids.contains(id))
            {
                continue;
            }
            notices
                .entry(sender)
                .or_default()
                .push(format!("message not delivered: {}", excerpt(&body)));
        }
        tx.commit()?;
    }

    for id in &closed_ids {
        let _ = purge_nudges_for_request(id);
    }

    for (sender, lines) in notices {
        // Nobody to tell: sidekar itself, the agent that left, or a sender that
        // has since gone too.
        if sender.is_empty()
            || sender == "sidekar"
            || sender == agent_name
            || !agent_is_registered(&sender).unwrap_or(false)
        {
            continue;
        }
        let body = notice_body(nick, agent_name, &lines);
        if enqueue_bus_message(&sender, "sidekar", &body, true, None).is_ok() {
            report.notified += 1;
        }
    }
    Ok(report)
}

/// Cancel what `sender` still has open with `recipient`, because `sender` is
/// the one stopping it.
///
/// Stopping an agent settles its mail, and each sender is told what went
/// unanswered. That is news to anyone except whoever did the stopping: told
/// "the agent you just stopped left with your request open", they learn only
/// what they did. Their requests are withdrawn first, so the notice goes to the
/// others alone. Returns how many were cancelled.
pub fn cancel_requests_before_stopping(sender: &str, recipient: &str) -> Result<usize> {
    let now = crate::message::epoch_secs();
    let mut cancelled = 0;
    for request in outbound_for_sender(sender)? {
        if request.transport_name == "broker" && request.transport_target == recipient {
            cancelled += cancel_outbound_request(&request.msg_id, now)?;
        }
    }
    Ok(cancelled)
}

/// The line a notice carries for one unanswered request.
fn unanswered_line(msg_id: &str, preview: &str) -> String {
    format!("request {msg_id} went unanswered: {}", excerpt(preview))
}

/// A departure the asker has already been told about some other way, by
/// `bus await` failing: close the request, and take it out of any notice still
/// waiting to be pasted.
///
/// Closing matters when the recipient crashed. Nothing has closed its request
/// yet, so the daemon's sweep would otherwise find it later and send the notice
/// after the wait already reported it. A notice about several things keeps its
/// other lines; one left with nothing to say is withdrawn.
pub fn settle_reported_departure(msg_id: &str) -> Result<()> {
    let Some(request) = outbound_request(msg_id)? else {
        return Ok(());
    };
    let now = crate::message::epoch_secs() as i64;
    let mut conn = open()?;
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE outbound_requests
         SET status = ?2, closed_at = COALESCE(closed_at, ?3)
         WHERE msg_id = ?1 AND status = ?4",
        params![
            msg_id,
            OUTBOUND_STATUS_RECIPIENT_GONE,
            now,
            OUTBOUND_STATUS_OPEN
        ],
    )?;
    tx.execute(
        "DELETE FROM pending_requests WHERE id = ?1",
        params![msg_id],
    )?;

    let marker = format!("request {msg_id} went unanswered:");
    let notices: Vec<(i64, String)> = {
        let mut stmt = tx.prepare(
            "SELECT id, body FROM bus_queue
             WHERE recipient = ?1 AND sender = 'sidekar'
               AND delivered_at = 0 AND claimed_at = 0 AND instr(body, ?2) > 0",
        )?;
        stmt.query_map(params![request.sender_name, marker], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    for (id, body) in notices {
        match without_line(&body, &marker) {
            Some(rest) => tx.execute(
                "UPDATE bus_queue SET body = ?2 WHERE id = ?1",
                params![id, rest],
            )?,
            None => tx.execute("DELETE FROM bus_queue WHERE id = ?1", params![id])?,
        };
    }
    tx.commit()?;
    Ok(())
}

/// `body` with the line containing `marker` removed, or `None` when that was
/// its only line and nothing is left worth sending.
pub(crate) fn without_line(body: &str, marker: &str) -> Option<String> {
    let mut lines = body.lines();
    let header = lines.next().unwrap_or_default();
    let rest: Vec<&str> = lines.filter(|l| !l.contains(marker)).collect();
    if rest.is_empty() {
        return None;
    }
    let mut out = header.to_string();
    for l in rest {
        out.push('\n');
        out.push_str(l);
    }
    Some(out)
}

/// The notice a sender gets, one per sender however much was withdrawn.
pub(crate) fn notice_body(nick: &str, agent_name: &str, lines: &[String]) -> String {
    let who = if nick.is_empty() || nick == agent_name {
        agent_name.to_string()
    } else {
        format!("{nick} ({agent_name})")
    };
    // Neither "before reading" nor "before answering" is true of every line —
    // an agent can read a request and leave without replying — so the header
    // claims only what is true of all of them, and each line says which it was.
    let mut body = format!(
        "[sidekar] {who} left the bus with this still open. \
         Nothing was passed to another agent:"
    );
    for line in lines {
        body.push_str("\n  - ");
        body.push_str(line);
    }
    body
}

fn excerpt(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= EXCERPT_CHARS {
        return flat;
    }
    let mut cut: String = flat.chars().take(EXCERPT_CHARS).collect();
    cut.push('…');
    cut
}

/// Names nobody holds that still have something waiting on them.
///
/// Two kinds of loose end: undelivered mail, and open requests. The second
/// matters on its own — an agent can *read* a request and then leave without
/// answering, and with nothing left in the queue a mail-only check would never
/// find it, leaving the request open forever and its sender never told.
///
/// The backstop for agents that left without settling their mail: a crash, or
/// an older sidekar that exits the old way. `grace_secs` keeps a sweep from
/// acting in the instant a live agent re-registers under its own name.
pub fn orphaned_mail_recipients(now: u64, grace_secs: u64) -> Result<Vec<String>> {
    let conn = open()?;
    let cutoff = now.saturating_sub(grace_secs) as i64;
    let mut stmt = conn.prepare(
        "SELECT q.recipient FROM bus_queue q
         WHERE q.delivered_at = 0 AND q.created_at < ?1
           AND NOT EXISTS (SELECT 1 FROM agents a WHERE a.name = q.recipient)
         UNION
         SELECT o.transport_target FROM outbound_requests o
         WHERE o.status = ?2 AND o.transport_name = 'broker' AND o.created_at < ?1
           AND NOT EXISTS (SELECT 1 FROM agents a WHERE a.name = o.transport_target)",
    )?;
    let names = stmt
        .query_map(params![cutoff, OUTBOUND_STATUS_OPEN], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(names)
}

/// The nick an agent last used, for notices about one that has already gone.
pub fn last_known_nick(agent_name: &str) -> Option<String> {
    let conn = open().ok()?;
    conn.query_row(
        "SELECT nick FROM agent_sessions WHERE agent_name = ?1 AND nick IS NOT NULL
         ORDER BY started_at DESC LIMIT 1",
        params![agent_name],
        |r| r.get::<_, String>(0),
    )
    .ok()
}
