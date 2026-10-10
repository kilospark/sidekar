//! Block until a request is answered, and hand back the answer.
//!
//! The bus delivers an answer by pasting it into the asker's terminal, and only
//! once the asker is idle. For an agent that asked and is now waiting, "idle"
//! means after the very turn that needed the answer — so the answer arrived a
//! turn late, and an agent that delegated work could not use the result in the
//! turn it delegated from.
//!
//! This reads the answer from the broker instead, keyed by the request's id.
//! Keyed by id rather than by who sent it, so a name reused by another agent
//! cannot hand back someone else's answer, and it does not care whether the
//! asker is busy.

use crate::broker::{self, BusReplyRecord};
use crate::message::Envelope;
use crate::utils::ExitWith;
use anyhow::{Result, bail};
use std::time::{Duration, Instant};

/// How often the broker is re-read. Answers are written by another process,
/// so this is a poll; a few queries a second is cheap and feels immediate.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How often a wait for an answer from another machine pulls for it.
const SYNC_PULL_INTERVAL: Duration = Duration::from_secs(3);

/// Default ceiling: long enough for a real piece of delegated work.
pub(crate) const DEFAULT_AWAIT: Duration = Duration::from_secs(600);

/// Exit status when the answer can no longer come: the recipient left, or the
/// request was withdrawn. Distinct from a timeout (1), where waiting longer
/// might still work.
pub const EXIT_NO_ANSWER: i32 = 2;

/// How a wait for an answer ended.
#[derive(Debug)]
pub enum AwaitOutcome {
    Answered(BusReplyRecord),
    /// The recipient left the bus without answering.
    RecipientGone {
        recipient: String,
    },
    /// The request was cancelled, so no answer is coming.
    Cancelled,
    TimedOut,
}

/// Who a request was sent to, pinned to the process that received it.
///
/// Pinned by pane because names are reused: "is `claude-app-2` still
/// registered" can be answered yes by an unrelated agent that took the name
/// after the recipient left.
#[derive(Debug, Clone)]
pub(crate) struct Recipient {
    pub name: String,
    pub pane: Option<String>,
}

impl Recipient {
    /// The request's recipient as the registry has it now, when it went over
    /// the local broker. Relay deliveries have no local presence to watch.
    fn of_request(msg_id: &str) -> Option<Self> {
        let request = broker::outbound_request(msg_id).ok()??;
        if request.transport_name != "broker" {
            return None;
        }
        let name = request.transport_target;
        let pane = broker::list_agents(None)
            .ok()?
            .into_iter()
            .find(|a| a.id.name == name)
            .and_then(|a| a.id.pane);
        Some(Self { name, pane })
    }

    fn still_here(&self) -> bool {
        match &self.pane {
            Some(pane) => broker::agent_for_pane_unique(pane)
                .ok()
                .flatten()
                .is_some_and(|a| a.id.name == self.name),
            None => broker::agent_is_registered(&self.name).unwrap_or(true),
        }
    }
}

/// Wait up to `timeout` for an answer to `msg_id`.
///
/// `recipient` is who to watch for leaving; `None` looks it up from the
/// request. Checked for an answer before and after noticing the recipient
/// gone, because an agent may answer and exit in the same instant.
pub(crate) async fn await_reply(
    msg_id: &str,
    recipient: Option<Recipient>,
    timeout: Duration,
) -> Result<AwaitOutcome> {
    let recipient = recipient.or_else(|| Recipient::of_request(msg_id));
    // An answer from another machine comes by bus sync. Pull for it here
    // rather than wait on the daemon's round: a machine that only asks may
    // have no daemon pulling at all.
    let synced = broker::outbound_request(msg_id)
        .ok()
        .flatten()
        .filter(|r| r.transport_name == crate::bus::BUS_SYNC_TRANSPORT)
        .and_then(|_| broker::bus_sync_account());
    let mut last_pull: Option<Instant> = None;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(uid) = &synced
            && last_pull.is_none_or(|t| t.elapsed() >= SYNC_PULL_INTERVAL)
        {
            last_pull = Some(Instant::now());
            if broker::pull_bus(uid).await.is_ok() {
                // The tombstones of whatever the pull delivered.
                let _ = broker::push_bus(uid, Duration::from_secs(5)).await;
            }
        }
        if let Some(reply) = first_reply(msg_id)? {
            return Ok(AwaitOutcome::Answered(reply));
        }
        match broker::outbound_request(msg_id)?.map(|r| r.status) {
            Some(s) if s == broker::OUTBOUND_STATUS_RECIPIENT_GONE => {
                return Ok(settle_gone(msg_id, recipient_name(&recipient, msg_id))?);
            }
            Some(s) if s == broker::OUTBOUND_STATUS_CANCELLED => {
                return Ok(match first_reply(msg_id)? {
                    Some(reply) => AwaitOutcome::Answered(reply),
                    None => AwaitOutcome::Cancelled,
                });
            }
            // Open, answered-but-not-yet-visible, or timed out. `timed_out`
            // is only the bus's five-minute warning to the sender; an answer
            // after it is still recorded, so it is no reason to stop.
            _ => {}
        }
        // The request row is swept after an hour, and an agent that crashed
        // leaves nobody to close it, so watch the recipient itself too.
        if let Some(r) = &recipient
            && !r.still_here()
        {
            return Ok(settle_gone(msg_id, r.name.clone())?);
        }
        if Instant::now() >= deadline {
            return Ok(AwaitOutcome::TimedOut);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn recipient_name(recipient: &Option<Recipient>, msg_id: &str) -> String {
    recipient
        .as_ref()
        .map(|r| r.name.clone())
        .or_else(|| {
            broker::outbound_request(msg_id)
                .ok()
                .flatten()
                .map(|r| r.recipient_name)
        })
        .unwrap_or_else(|| "the recipient".to_string())
}

fn settle_gone(msg_id: &str, recipient: String) -> Result<AwaitOutcome> {
    Ok(match first_reply(msg_id)? {
        Some(reply) => AwaitOutcome::Answered(reply),
        None => AwaitOutcome::RecipientGone { recipient },
    })
}

fn first_reply(msg_id: &str) -> Result<Option<BusReplyRecord>> {
    Ok(broker::replies_for_request(msg_id)?.into_iter().next())
}

/// Finish with an answer that was read here rather than from the pane.
///
/// Two things the normal paste path would have done, or would still do:
/// the request is closed as answered — an answer can land before the request
/// row is written, when a delegate is fast — and the copy queued for the
/// asker's pane is withdrawn, so the asker does not read the same answer
/// again a turn later.
pub(crate) fn consume_reply(msg_id: &str, reply: &BusReplyRecord) {
    if let Ok(envelope) = serde_json::from_str::<Envelope>(&reply.envelope_json) {
        let _ = broker::record_reply(msg_id, &envelope);
    }
    let _ = broker::withdraw_undelivered_envelope(&reply.reply_msg_id);
}

/// Turn an outcome into the answer text, or an error with the right status.
pub(crate) fn answer_or_exit(
    msg_id: &str,
    outcome: AwaitOutcome,
    timeout: Duration,
) -> Result<String> {
    match outcome {
        AwaitOutcome::Answered(reply) => {
            consume_reply(msg_id, &reply);
            Ok(reply.message)
        }
        AwaitOutcome::RecipientGone { recipient } => {
            // The caller is being told right here, so it must not be told
            // again by the notice pasted into its pane minutes later.
            let _ = broker::settle_reported_departure(msg_id);
            Err(ExitWith::new(
                EXIT_NO_ANSWER,
                format!("{recipient} left the bus without answering request {msg_id}."),
            )
            .into())
        }
        AwaitOutcome::Cancelled => Err(ExitWith::new(
            EXIT_NO_ANSWER,
            format!("Request {msg_id} was cancelled; no answer is coming."),
        )
        .into()),
        AwaitOutcome::TimedOut => bail!(
            "No answer to request {msg_id} within {}. `sidekar bus await {msg_id}` keeps waiting.",
            describe(timeout)
        ),
    }
}

/// `bus await <msg-id> [--timeout <duration>]`.
pub async fn cmd_await(ctx: &mut crate::AppContext, args: &[String]) -> Result<()> {
    const USAGE: &str = "Usage: sidekar bus await <msg-id> [--timeout <duration, e.g. 90s, 10m>]";
    let mut msg_id: Option<&str> = None;
    let mut timeout = DEFAULT_AWAIT;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if let Some(v) = arg.strip_prefix("--timeout=") {
            timeout = parse_duration(v)?;
        } else if arg == "--timeout" {
            i += 1;
            timeout = parse_duration(args.get(i).map(String::as_str).unwrap_or_default())?;
        } else if arg.starts_with('-') {
            bail!("Unknown flag {arg}. {USAGE}");
        } else if msg_id.is_none() {
            msg_id = Some(arg);
        } else {
            bail!("Unexpected argument {arg}. {USAGE}");
        }
        i += 1;
    }
    let Some(msg_id) = msg_id else {
        bail!("{USAGE}")
    };

    // A typo would otherwise wait out the whole timeout for an id nothing
    // will ever answer.
    if broker::outbound_request(msg_id)?.is_none() && first_reply(msg_id)?.is_none() {
        bail!(
            "No request with id {msg_id}. Ids come from `sidekar bus send` \
             (add --id-only to get just the id) or `sidekar bus requests`."
        );
    }

    let outcome = await_reply(msg_id, None, timeout).await?;
    let answer = answer_or_exit(msg_id, outcome, timeout)?;
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(answer))?
    );
    Ok(())
}

/// `90s`, `10m`, `1h`, or bare seconds.
pub(crate) fn parse_duration(v: &str) -> Result<Duration> {
    let v = v.trim();
    let (digits, scale) = match v.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        None => (v, 1),
        Some((at, _)) => (
            &v[..at],
            match &v[at..] {
                "s" => 1,
                "m" => 60,
                "h" => 3600,
                _ => bail!("Invalid duration {v:?}. Use 90s, 10m, 1h, or whole seconds."),
            },
        ),
    };
    let n: u64 = digits.parse().map_err(|_| {
        anyhow::anyhow!("Invalid duration {v:?}. Use 90s, 10m, 1h, or whole seconds.")
    })?;
    if n == 0 {
        bail!("Invalid duration {v:?}: must be more than zero.");
    }
    Ok(Duration::from_secs(n * scale))
}

pub(crate) fn describe(d: Duration) -> String {
    let s = d.as_secs();
    if s % 3600 == 0 {
        format!("{}h", s / 3600)
    } else if s % 60 == 0 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests;
