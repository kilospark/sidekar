//! The bus across an account's machines: agent presence and messages, carried
//! by the same encrypted, versioned record sync as kv and totp, on its own
//! channel. Design and trade-offs: `context/bus-sync.md`.
//!
//! - **Presence.** Each machine publishes its long-lived agents as `agent`
//!   records, named `<device>\0<agent name>`. The daemon reconciles them every
//!   tick (see [`reconcile_presence`]): publishes new agents, republishes each
//!   every [`HEARTBEAT_SECS`], and tombstones the ones that left. Another
//!   machine's agent counts as present until [`PRESENCE_TTL_SECS`] pass
//!   without a heartbeat, so a machine that sleeps or crashes drops out on
//!   its own.
//! - **Messages.** A message for another machine's agent is a `bus` record
//!   named by the message id and addressed to that machine. Its recipient
//!   delivers it into the local queue, as the relay does for a tunnelled
//!   message, and tombstones the record.
//!
//! This module is the data side, synchronous and local. The network side,
//! push and pull, lives in `sync` with the rest of the sync channel code.

use super::*;
use crate::message::{Envelope, MessageKind};

pub(crate) const KIND_AGENT: &str = "agent";
pub(crate) const KIND_BUS: &str = "bus";

/// How often a machine republishes each of its agents.
pub(crate) const HEARTBEAT_SECS: i64 = 120;

/// How long after its last heartbeat another machine's agent is taken as gone:
/// two missed heartbeats and a margin for clocks that disagree.
pub(crate) const PRESENCE_TTL_SECS: i64 = 300;

/// How long the bookkeeping for a delivered message, or an unsent one, is kept.
const KEEP_SECS: i64 = 7 * 24 * 3600;

/// Whether `kind` travels on the bus channel rather than the secrets one.
pub(crate) fn is_bus_kind(kind: &str) -> bool {
    kind == KIND_AGENT || kind == KIND_BUS
}

pub(crate) fn agent_record_id(device_id: &str, name: &str) -> String {
    format!("{device_id}\u{0}{name}")
}

fn split_agent_record_id(record_id: &str) -> Option<(&str, &str)> {
    record_id.split_once('\u{0}')
}

/// Whether a registration is published to the account's other machines. A
/// `cli-` pane is one command, from a shell or a `spawn --wait`, gone before
/// anyone elsewhere could address it.
pub(crate) fn publishes(pane: Option<&str>) -> bool {
    pane.is_some_and(|p| !p.is_empty() && !p.starts_with("cli-"))
}

pub(crate) fn hostname() -> String {
    let mut buf = [0u8; 256];
    let ret = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if ret != 0 {
        return "unknown".to_string();
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).to_string()
}

/// An agent as another machine sees it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AgentPayload {
    name: String,
    #[serde(default)]
    nick: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    agent_type: Option<String>,
    hostname: String,
    device_id: String,
    published_at: i64,
    /// What the agent is doing, for `bus wait`, `bus explain` and `agents`
    /// elsewhere. Absent from releases before it.
    #[serde(default)]
    activity: Option<RemoteActivity>,
    /// Requests waiting on its answer.
    #[serde(default)]
    pending: i64,
    /// When the agent last showed signs of life on its machine.
    #[serde(default)]
    last_active_at: Option<i64>,
}

/// An agent's activity as its own machine read it when publishing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RemoteActivity {
    pub state: String,
    /// The reading's own time, on the publishing machine's clock.
    pub at: u64,
    /// Whether the reading was current when published. A change to any of
    /// these republishes the agent, so a fresh reading stays true for as long
    /// as the agent's presence does.
    pub fresh: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub settled_at: Option<u64>,
    #[serde(default)]
    pub seen_at: Option<u64>,
}

impl RemoteActivity {
    /// The reading as this machine should treat it now: a fresh one is current
    /// while the agent's presence is, so it is dated now; a stale one keeps its
    /// own, old, time.
    pub(crate) fn detail(&self, now: u64) -> super::ActivityDetail {
        super::ActivityDetail {
            state: crate::activity::ActivityState::parse(&self.state),
            at: if self.fresh { now } else { self.at },
            reason: self.reason.clone(),
            settled_at: self.settled_at,
            seen_at: self.seen_at,
        }
    }
}

/// What the agent's presence record says about it now, and what to compare a
/// published one against to tell whether it needs publishing again.
fn local_activity(conn: &Connection, name: &str) -> Result<Option<(RemoteActivity, i64, Option<i64>)>> {
    let Some(detail) = conn
        .query_row(
            "SELECT activity_state, activity_at, activity_reason, settled_at, seen_at, last_seen_at
               FROM agents WHERE name = ?1",
            params![name],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(None);
    };
    let (state, at, reason, settled_at, seen_at, last_seen) = detail;
    let snapshot = crate::activity::ActivitySnapshot {
        state: crate::activity::ActivityState::parse(&state),
        at: at.max(0) as u64,
    };
    let pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_requests WHERE recipient_name = ?1",
        params![name],
        |r| r.get(0),
    )?;
    Ok(Some((
        RemoteActivity {
            state,
            at: at.max(0) as u64,
            fresh: !snapshot.is_stale(),
            reason,
            settled_at: settled_at.map(|v| v.max(0) as u64),
            seen_at: seen_at.map(|v| v.max(0) as u64),
        },
        pending,
        last_seen,
    )))
}

/// What has to change for an agent to be published again before its
/// heartbeat: its state, whether that is current, its last finish, and how
/// many requests wait on it. Not the reading's time, which moves constantly.
fn presence_signature(conn: &Connection, name: &str) -> Result<Option<String>> {
    Ok(local_activity(conn, name)?.map(|(a, pending, _)| {
        format!(
            "{}|{}|{}|{}",
            a.state,
            a.fresh,
            a.settled_at.unwrap_or(0),
            pending
        )
    }))
}

/// A message to an agent on another machine.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct MessagePayload {
    /// The machine that delivers it.
    pub to_device: String,
    pub from_device: String,
    pub recipient: String,
    pub sender: String,
    /// The text pasted into the recipient's pane.
    pub body: String,
    #[serde(default)]
    pub envelope_json: Option<String>,
    pub created_at: i64,
    /// Set on a bounce: the id of the message this machine sent that could not
    /// be delivered, which closes its request here so `bus await` fails rather
    /// than waits. A release before bounces reads one as a plain message to the
    /// asker, which is why a bounce carries no envelope.
    #[serde(default)]
    pub bounce_of: Option<String>,
    /// Why it could not be delivered.
    #[serde(default)]
    pub undeliverable: Option<String>,
}

/// The plaintext sync payload for a dirty bus-channel record.
pub(crate) fn sync_payload(conn: &Connection, kind: &str, record_id: &str) -> Result<String> {
    match kind {
        KIND_AGENT => {
            let (device_id, name) = split_agent_record_id(record_id)
                .ok_or_else(|| anyhow!("malformed agent sync record id"))?;
            let mut agent = conn
                .query_row(
                    "SELECT nick, session, cwd, agent_type FROM agents WHERE name = ?1",
                    params![name],
                    |r| {
                        Ok(AgentPayload {
                            name: name.to_string(),
                            nick: r.get(0)?,
                            channel: r.get(1)?,
                            cwd: r.get(2)?,
                            agent_type: r.get(3)?,
                            hostname: hostname(),
                            device_id: device_id.to_string(),
                            published_at: crate::message::epoch_secs() as i64,
                            activity: None,
                            pending: 0,
                            last_active_at: None,
                        })
                    },
                )
                .optional()?
                .ok_or_else(|| anyhow!("agent '{name}' left before it was published"))?;
            if let Some((activity, pending, last_active)) = local_activity(conn, name)? {
                agent.activity = Some(activity);
                agent.pending = pending;
                agent.last_active_at = last_active;
            }
            Ok(serde_json::to_string(&agent)?)
        }
        KIND_BUS => conn
            .query_row(
                "SELECT payload FROM bus_outbox WHERE msg_id = ?1",
                params![record_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("bus message '{record_id}' has no outbox entry")),
        other => bail!("not a bus sync kind: {other}"),
    }
}

/// A message once pushed needs no outbox entry.
pub(crate) fn pushed(conn: &Connection, kind: &str, record_id: &str) -> Result<()> {
    if kind == KIND_BUS {
        conn.execute(
            "DELETE FROM bus_outbox WHERE msg_id = ?1",
            params![record_id],
        )?;
    }
    Ok(())
}

/// Bring this machine's published agents in line with the ones registered
/// here: publish the new, republish any due a heartbeat, tombstone the gone.
/// Returns how many records it marked for push.
pub(crate) fn reconcile_presence(conn: &Connection, uid: &str, device_id: &str) -> Result<usize> {
    let now = crate::message::epoch_secs() as i64;
    let local: std::collections::HashSet<String> = {
        let mut stmt = conn.prepare("SELECT name, pane_unique_id FROM agents")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        rows.filter_map(|r| r.ok())
            .filter(|(_, pane)| publishes(pane.as_deref()))
            .map(|(name, _)| name)
            .collect()
    };
    // (name, deleted, updated_at) of what this machine has published.
    let published: Vec<(String, bool, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT record_id, deleted, updated_at FROM sync_state WHERE user_id = ?1 AND kind = ?2",
        )?;
        let rows = stmt.query_map(params![uid, KIND_AGENT], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)? != 0,
                r.get::<_, i64>(2)?,
            ))
        })?;
        rows.filter_map(|r| r.ok())
            .filter_map(|(rid, deleted, at)| {
                let (dev, name) = split_agent_record_id(&rid)?;
                (dev == device_id).then(|| (name.to_string(), deleted, at))
            })
            .collect()
    };

    let mut marked = 0;
    for name in &local {
        let signature = presence_signature(conn, name)?.unwrap_or_default();
        let published_signature: Option<String> = conn
            .query_row(
                "SELECT signature FROM bus_presence_published WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()?;
        let due = match published.iter().find(|(n, _, _)| n == name) {
            None => true,
            Some((_, deleted, at)) => {
                *deleted
                    || now - at >= HEARTBEAT_SECS
                    || published_signature.as_deref() != Some(signature.as_str())
            }
        };
        if due {
            mark_dirty(
                conn,
                uid,
                KIND_AGENT,
                &agent_record_id(device_id, name),
                false,
            )?;
            conn.execute(
                "INSERT INTO bus_presence_published (name, signature) VALUES (?1, ?2)
                 ON CONFLICT(name) DO UPDATE SET signature = ?2",
                params![name, signature],
            )?;
            marked += 1;
        }
    }
    for (name, deleted, _) in &published {
        if !deleted && !local.contains(name) {
            conn.execute(
                "DELETE FROM bus_presence_published WHERE name = ?1",
                params![name],
            )?;
            mark_dirty(
                conn,
                uid,
                KIND_AGENT,
                &agent_record_id(device_id, name),
                true,
            )?;
            marked += 1;
        }
    }
    Ok(marked)
}

/// Whether the daemon's round should pull: while this machine has an agent
/// another could message, and for a while after the last one left. Other
/// machines go on addressing an agent until its tombstone or silence reaches
/// them, and what they sent meanwhile has to be pulled to be bounced back,
/// not left to expire unanswered.
pub(crate) fn should_pull(conn: &Connection, uid: &str) -> Result<bool> {
    if has_published_agents(conn)? {
        return Ok(true);
    }
    let since = crate::message::epoch_secs() as i64 - 2 * PRESENCE_TTL_SECS;
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_state
                        WHERE user_id = ?1 AND kind = ?2 AND deleted = 1 AND updated_at > ?3)",
        params![uid, KIND_AGENT, since],
        |r| r.get(0),
    )?)
}

/// Whether this machine has an agent published to the others, so something
/// here can be messaged and the daemon should keep pulling.
pub(crate) fn has_published_agents(conn: &Connection) -> Result<bool> {
    let mut stmt = conn.prepare("SELECT pane_unique_id FROM agents")?;
    let panes = stmt.query_map([], |r| r.get::<_, Option<String>>(0))?;
    Ok(panes
        .filter_map(|p| p.ok())
        .any(|p| publishes(p.as_deref())))
}

/// Queue `body` for `recipient` on the machine `to_device`. Durable before any
/// network attempt: it goes out with the next push, whoever makes it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn queue_remote_message(
    conn: &Connection,
    uid: &str,
    from_device: &str,
    to_device: &str,
    recipient: &str,
    sender: &str,
    body: &str,
    envelope: Option<&Envelope>,
) -> Result<String> {
    let msg_id = envelope
        .map(|e| e.id.clone())
        .unwrap_or_else(crate::message::gen_msg_id);
    let payload = MessagePayload {
        to_device: to_device.to_string(),
        from_device: from_device.to_string(),
        recipient: recipient.to_string(),
        sender: sender.to_string(),
        body: body.to_string(),
        envelope_json: envelope.map(serde_json::to_string).transpose()?,
        created_at: crate::message::epoch_secs() as i64,
        bounce_of: None,
        undeliverable: None,
    };
    queue_payload(conn, uid, &msg_id, &payload)?;
    Ok(msg_id)
}

/// Put a message in the outbox and mark it for the next push.
fn queue_payload(conn: &Connection, uid: &str, msg_id: &str, payload: &MessagePayload) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO bus_outbox (msg_id, user_id, payload, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![msg_id, uid, serde_json::to_string(payload)?, payload.created_at],
    )?;
    mark_dirty(conn, uid, KIND_BUS, msg_id, false)?;
    Ok(())
}

/// An agent another machine on the account has published.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct RemoteAgent {
    pub device_id: String,
    pub hostname: String,
    pub name: String,
    pub nick: Option<String>,
    pub channel: Option<String>,
    pub cwd: Option<String>,
    pub agent_type: Option<String>,
    pub published_at: i64,
    /// Absent when its machine runs a release from before activity was
    /// published.
    pub activity: Option<RemoteActivity>,
    pub pending: i64,
    pub last_active_at: Option<i64>,
}

impl RemoteAgent {
    pub(crate) fn record_id(&self) -> String {
        agent_record_id(&self.device_id, &self.name)
    }

    /// `nick (name)`, or the name, and the machine it is on.
    pub(crate) fn label(&self) -> String {
        let who = match &self.nick {
            Some(nick) => format!("{nick} ({})", self.name),
            None => self.name.clone(),
        };
        format!("{who} on \"{}\"", self.hostname)
    }
}

/// The other machines' agents still present: heard from within
/// [`PRESENCE_TTL_SECS`].
pub(crate) fn live_remote_agents(conn: &Connection, uid: &str) -> Result<Vec<RemoteAgent>> {
    let cutoff = crate::message::epoch_secs() as i64 - PRESENCE_TTL_SECS;
    let mut stmt = conn.prepare(
        "SELECT device_id, hostname, name, nick, channel, cwd, agent_type, published_at,
                activity_json, pending, last_active_at
           FROM remote_agents WHERE user_id = ?1 AND published_at > ?2
          ORDER BY hostname, name",
    )?;
    let rows = stmt.query_map(params![uid, cutoff], |r| {
        Ok(RemoteAgent {
            device_id: r.get(0)?,
            hostname: r.get(1)?,
            name: r.get(2)?,
            nick: r.get(3)?,
            channel: r.get(4)?,
            cwd: r.get(5)?,
            agent_type: r.get(6)?,
            published_at: r.get(7)?,
            activity: r
                .get::<_, Option<String>>(8)?
                .and_then(|j| serde_json::from_str(&j).ok()),
            pending: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
            last_active_at: r.get(10)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The other machine's agent that `record_id` names, while it is present.
pub(crate) fn remote_agent_by_record(
    conn: &Connection,
    uid: &str,
    record_id: &str,
) -> Result<Option<RemoteAgent>> {
    Ok(live_remote_agents(conn, uid)?
        .into_iter()
        .find(|a| a.record_id() == record_id))
}

/// The agent on another machine that `target` names, for a command that found
/// no agent of that name here: asks the server first when this machine has not
/// heard of one, so an agent started over there a moment ago is found. `None`
/// when bus sync is off or nothing matches; the account comes with it.
pub(crate) fn lookup_remote_agent(target: &str) -> Result<Option<(String, RemoteAgent)>> {
    let Some(uid) = bus_sync_account() else {
        return Ok(None);
    };
    let find = |uid: &str| find_remote_agent(&open()?, uid, target);
    if let Some(found) = find(&uid)? {
        return Ok(Some((uid, found)));
    }
    let pull_uid = uid.clone();
    let _ = super::sync::run_blocking(async move { super::sync::pull_bus(&pull_uid).await });
    Ok(find(&uid)?.map(|a| (uid, a)))
}

/// The agent on another machine that `target` names: by bus name or nick,
/// optionally `<name>@<host>` to pick one machine. Two machines with an agent
/// of the same name (the same project checked out on both, say) need the host.
pub(crate) fn find_remote_agent(
    conn: &Connection,
    uid: &str,
    target: &str,
) -> Result<Option<RemoteAgent>> {
    let target = crate::message::parse_target(target);
    let (name, host) = match target.rsplit_once('@') {
        Some((n, h)) if !n.is_empty() && !h.is_empty() => (n, Some(h)),
        _ => (target.as_str(), None),
    };
    // A host name, its first label, or a prefix of the machine's device id
    // (at least 4 characters) for two machines that share a host name.
    let host_matches = |a: &RemoteAgent| match host {
        None => true,
        Some(h) => {
            let lower = h.to_ascii_lowercase();
            let mine = a.hostname.to_ascii_lowercase();
            mine == lower
                || mine.split('.').next() == Some(lower.as_str())
                || (h.len() >= 4 && a.device_id.starts_with(h))
        }
    };
    let matches: Vec<RemoteAgent> = live_remote_agents(conn, uid)?
        .into_iter()
        .filter(|a| a.name == name || a.nick.as_deref() == Some(name))
        .filter(host_matches)
        .collect();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.into_iter().next()),
        _ => {
            let shared_host =
                |a: &RemoteAgent| matches.iter().filter(|b| b.hostname == a.hostname).count() > 1;
            let options: Vec<String> = matches
                .iter()
                .map(|a| {
                    if shared_host(a) {
                        format!("{}@{}", a.name, device_prefix(&a.device_id))
                    } else {
                        format!("{}@{}", a.name, a.hostname)
                    }
                })
                .collect();
            bail!(
                "\"{target}\" names agents on more than one machine; pick one: {}",
                options.join(", ")
            )
        }
    }
}

/// Enough of a device id to tell machines apart, without characters a shell
/// would need quoted.
fn device_prefix(device_id: &str) -> String {
    let clean: String = device_id
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    if clean.len() >= 8 {
        clean[..8].to_string()
    } else {
        device_id.chars().take(8).collect()
    }
}

/// The machine a request came from, when another machine sent it.
pub(crate) fn origin_device(conn: &Connection, msg_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT device_id FROM bus_remote_origin WHERE msg_id = ?1",
            params![msg_id],
            |r| r.get(0),
        )
        .optional()?)
}

/// Apply one record pulled from the bus channel. Returns whether it changed
/// anything on this machine.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_record(
    conn: &Connection,
    uid: &str,
    device_id: &str,
    kind: &str,
    record_id: &str,
    ciphertext: &str,
    version: i64,
    deleted: bool,
) -> Result<bool> {
    match kind {
        KIND_AGENT => {
            let Some((dev, _)) = split_agent_record_id(record_id) else {
                bail!("malformed agent sync record id");
            };
            if dev == device_id {
                return Ok(false); // our own, back from the server
            }
            if deleted {
                let n = conn.execute(
                    "DELETE FROM remote_agents WHERE record_id = ?1",
                    params![record_id],
                )?;
                return Ok(n > 0);
            }
            let a: AgentPayload = serde_json::from_str(&decrypt_record(ciphertext)?)
                .context("invalid agent sync payload")?;
            let n = conn.execute(
                "INSERT INTO remote_agents (record_id, user_id, device_id, hostname, name, nick,
                                            channel, cwd, agent_type, published_at, version,
                                            activity_json, pending, last_active_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                 ON CONFLICT(record_id) DO UPDATE SET
                    user_id = ?2, hostname = ?4, name = ?5, nick = ?6, channel = ?7, cwd = ?8,
                    agent_type = ?9, published_at = ?10, version = ?11, activity_json = ?12,
                    pending = ?13, last_active_at = ?14
                  WHERE ?11 > remote_agents.version",
                params![
                    record_id,
                    uid,
                    dev,
                    a.hostname,
                    a.name,
                    a.nick,
                    a.channel,
                    a.cwd,
                    a.agent_type,
                    // A sender clock running ahead would keep its agents
                    // listed past the TTL; one running behind only drops them
                    // a little early.
                    a.published_at.min(crate::message::epoch_secs() as i64),
                    version,
                    a.activity.as_ref().map(serde_json::to_string).transpose()?,
                    a.pending,
                    a.last_active_at
                ],
            )?;
            Ok(n > 0)
        }
        KIND_BUS => {
            if deleted {
                return Ok(false);
            }
            let msg: MessagePayload = serde_json::from_str(&decrypt_record(ciphertext)?)
                .context("invalid bus message sync payload")?;
            if msg.to_device != device_id {
                return Ok(false);
            }
            // Several processes pull this channel: the daemon's round, `bus
            // await`, `bus send` and `bus who --all`. Whichever claims the
            // record delivers it; any other pull, now or a re-pull of the
            // overlap window later, finds the claim and skips it.
            if !claim_message(conn, uid, record_id, version)? {
                return Ok(false);
            }
            if let Err(e) = deliver(conn, uid, device_id, &msg) {
                // Let a later pull try again rather than lose it.
                conn.execute(
                    "DELETE FROM sync_state WHERE user_id = ?1 AND kind = ?2 AND record_id = ?3 AND dirty = 1",
                    params![uid, KIND_BUS, record_id],
                )?;
                return Err(e);
            }
            Ok(true)
        }
        other => bail!("not a bus sync kind: {other}"),
    }
}

/// Claim a pulled message for delivery here: record it, already tombstoned
/// for push, in one write transaction. True only for the one caller whose
/// insert created the row; checking first and recording after delivery let
/// two concurrent pulls both deliver it.
fn claim_message(conn: &Connection, uid: &str, record_id: &str, version: i64) -> Result<bool> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let inserted = conn.execute(
        "INSERT INTO sync_state (user_id, kind, record_id, version, deleted, dirty, updated_at)
         VALUES (?1, ?2, ?3, ?4, 1, 1, ?5)
         ON CONFLICT(user_id, kind, record_id) DO NOTHING",
        params![
            uid,
            KIND_BUS,
            record_id,
            version + 1,
            crate::message::epoch_secs() as i64
        ],
    );
    match inserted {
        Ok(n) => {
            conn.execute_batch("COMMIT")?;
            Ok(n == 1)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e.into())
        }
    }
}

fn decrypt_record(ciphertext: &str) -> Result<String> {
    let key = get_encryption_key().context("no account key loaded to read bus sync records")?;
    super::encryption::sync_decrypt(&key, ciphertext)
}

/// Hand a message from another machine to its recipient here, with the same
/// bookkeeping the relay does for a tunnelled one: a request is pending until
/// answered, an answer is recorded against its request for `bus await`.
fn deliver(conn: &Connection, uid: &str, device_id: &str, msg: &MessagePayload) -> Result<()> {
    if let Some(original) = msg.bounce_of.as_deref() {
        return receive_bounce(conn, msg, original);
    }
    let envelope: Option<Envelope> = msg
        .envelope_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .context("invalid envelope in bus message")?;
    let here = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agents WHERE name = ?1)",
        params![msg.recipient],
        |r| r.get::<_, bool>(0),
    )?;
    let is_answer = envelope
        .as_ref()
        .is_some_and(|e| e.kind == MessageKind::Response && e.reply_to.is_some());
    if !here && !is_answer {
        // Nobody here to read it, and queueing it under the name would hand
        // it to the next agent to take that name. Tell the sender instead.
        try_log_event(
            "warn",
            "bus-sync",
            "message from another machine for an agent no longer here; bounced",
            Some(&format!(
                "recipient={} sender={}",
                msg.recipient, msg.sender
            )),
        );
        if let Some(e) = &envelope {
            bounce(conn, uid, device_id, msg, e)?;
        }
        return Ok(());
    }
    if let Some(ref e) = envelope {
        match e.kind {
            MessageKind::Request | MessageKind::Handoff => {
                if e.requires_reply() {
                    set_pending(e)?;
                } else {
                    let _ = dismiss_terminal_ack_request(&e.id);
                }
                // Where an answer goes, even after the asker has left the bus.
                conn.execute(
                    "INSERT OR REPLACE INTO bus_remote_origin (msg_id, device_id, created_at)
                     VALUES (?1, ?2, ?3)",
                    params![e.id, msg.from_device, crate::message::epoch_secs() as i64],
                )?;
            }
            MessageKind::Response => {
                if let Some(reply_to) = e.reply_to.as_deref() {
                    record_reply(reply_to, e)?;
                }
            }
            MessageKind::Fyi => {}
        }
    }
    // An answer whose asker has left was recorded above for `bus await`.
    if here {
        enqueue_bus_message(
            &msg.recipient,
            &msg.sender,
            &msg.body,
            true,
            envelope.as_ref(),
        )?;
    }
    Ok(())
}

/// Send `msg`, which could not be delivered here, back to the machine it came
/// from: a bounce naming it, which closes its request there.
fn bounce(
    conn: &Connection,
    uid: &str,
    device_id: &str,
    msg: &MessagePayload,
    envelope: &Envelope,
) -> Result<()> {
    let reason = format!(
        "{} is no longer on \"{}\"",
        msg.recipient,
        hostname()
    );
    let excerpt: String = envelope.message.chars().take(120).collect();
    let payload = MessagePayload {
        to_device: msg.from_device.clone(),
        from_device: device_id.to_string(),
        recipient: msg.sender.clone(),
        sender: "sidekar".to_string(),
        body: format!(
            "[sidekar] Message {} to {} was not delivered: {reason}. It said: {excerpt}",
            envelope.id, msg.recipient
        ),
        envelope_json: None,
        created_at: crate::message::epoch_secs() as i64,
        bounce_of: Some(envelope.id.clone()),
        undeliverable: Some(reason),
    };
    queue_payload(conn, uid, &crate::message::gen_msg_id(), &payload)
}

/// A bounce of a message this machine sent: close its request, so `bus await`
/// reports it undelivered rather than waiting, and tell the sender if it is
/// still here.
fn receive_bounce(conn: &Connection, msg: &MessagePayload, original: &str) -> Result<()> {
    let now = crate::message::epoch_secs() as i64;
    conn.execute(
        "UPDATE outbound_requests
            SET status = ?2, closed_at = COALESCE(closed_at, ?3)
          WHERE msg_id = ?1 AND status IN (?4, ?5)",
        params![
            original,
            OUTBOUND_STATUS_RECIPIENT_GONE,
            now,
            OUTBOUND_STATUS_OPEN,
            OUTBOUND_STATUS_TIMED_OUT
        ],
    )?;
    conn.execute("DELETE FROM pending_requests WHERE id = ?1", params![original])?;
    let _ = purge_nudges_for_request(original);
    let here = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agents WHERE name = ?1)",
        params![msg.recipient],
        |r| r.get::<_, bool>(0),
    )?;
    if here {
        enqueue_bus_message(&msg.recipient, &msg.sender, &msg.body, true, None)?;
    }
    try_log_event(
        "info",
        "bus-sync",
        "a message to another machine bounced",
        Some(&format!(
            "msg={original} reason={}",
            msg.undeliverable.as_deref().unwrap_or("not delivered")
        )),
    );
    Ok(())
}

/// Drop bus-sync bookkeeping nothing needs any more. Run by the daemon's sweep.
pub(crate) fn prune(conn: &Connection) -> Result<()> {
    let now = crate::message::epoch_secs() as i64;
    let cutoff = now - KEEP_SECS;
    // A message that could not be pushed in a week is given up on, with its
    // sync row, or every push would retry it.
    conn.execute(
        "DELETE FROM sync_state WHERE kind = ?1 AND record_id IN
            (SELECT msg_id FROM bus_outbox WHERE created_at < ?2)",
        params![KIND_BUS, cutoff],
    )?;
    conn.execute(
        "DELETE FROM bus_outbox WHERE created_at < ?1",
        params![cutoff],
    )?;
    conn.execute(
        "DELETE FROM sync_state WHERE kind = ?1 AND dirty = 0 AND updated_at < ?2",
        params![KIND_BUS, cutoff],
    )?;
    // One per agent this machine ever published.
    conn.execute(
        "DELETE FROM sync_state WHERE kind = ?1 AND deleted = 1 AND dirty = 0 AND updated_at < ?2",
        params![KIND_AGENT, cutoff],
    )?;
    conn.execute(
        "DELETE FROM bus_remote_origin WHERE created_at < ?1",
        params![cutoff],
    )?;
    conn.execute(
        "DELETE FROM remote_agents WHERE published_at < ?1",
        params![now - 24 * 3600],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
