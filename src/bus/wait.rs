//! Block until another agent reaches an activity state.
//!
//! `bus send` is fire-and-forget from the caller's side: it queues a tracked
//! request and nudges until the recipient replies, but the sender's own turn
//! carries on immediately. That is the right default for a handoff and the
//! wrong one for a dependency — "start the reviewer, then wait for it to
//! actually be ready before prompting it" has no expression on the bus.
//!
//! This adds the missing half. It reads the same [`ActivityState`] the PTY
//! wrapper already publishes for nudge gating, so nothing new is observed;
//! the state was there and just had no way to be waited on.

use crate::activity::{ActivitySnapshot, ActivityState};
use crate::app_context::AppContext;
use crate::broker;
use anyhow::{Result, bail};
use std::time::Duration;

/// How often the agent's published state is re-read.
///
/// Activity is written to SQLite by another process, so this is a poll rather
/// than a subscription. Fast enough to feel immediate, slow enough that a long
/// wait is not thousands of queries.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Default ceiling, matching the scale of an agent turn rather than a command.
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// What the caller is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitUntil {
    /// The agent stopped needing the CPU: ready for input, or parked on a
    /// question. Both mean the caller can act; the distinction is in the
    /// reported state, not in whether to keep waiting.
    Settled,
    /// One specific state.
    State(ActivityState),
}

impl WaitUntil {
    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "settled" => Self::Settled,
            "idle" => Self::State(ActivityState::Idle),
            "needs-input" | "needs_input" => Self::State(ActivityState::NeedsInput),
            "working" | "agent-working" | "agent_working" => {
                Self::State(ActivityState::AgentWorking)
            }
            "user-typing" | "user_typing" => Self::State(ActivityState::UserTyping),
            other => bail!(
                "Invalid --until={other}. Valid: settled, idle, needs-input, working, user-typing"
            ),
        })
    }

    fn satisfied_by(self, state: ActivityState) -> bool {
        match self {
            Self::Settled => matches!(state, ActivityState::Idle | ActivityState::NeedsInput),
            Self::State(want) => state == want,
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Self::Settled => "settled",
            Self::State(s) => s.as_str(),
        }
    }
}

/// `bus wait <agent> [--until <state>] [--timeout <ms>]`.
pub async fn cmd_wait(
    state: &crate::bus::SidekarBusState,
    ctx: &mut AppContext,
    args: &[String],
) -> Result<()> {
    let mut target: Option<&str> = None;
    let mut until = WaitUntil::Settled;
    let mut timeout_ms = DEFAULT_TIMEOUT_MS;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if let Some(v) = arg.strip_prefix("--until=") {
            until = WaitUntil::parse(v)?;
        } else if arg == "--until" {
            i += 1;
            let v = args.get(i).map(String::as_str).unwrap_or_default();
            until = WaitUntil::parse(v)?;
        } else if let Some(v) = arg.strip_prefix("--timeout=") {
            timeout_ms = parse_timeout(v)?;
        } else if arg == "--timeout" {
            i += 1;
            let v = args.get(i).map(String::as_str).unwrap_or_default();
            timeout_ms = parse_timeout(v)?;
        } else if arg.starts_with('-') {
            bail!(
                "Unknown flag {arg}. Usage: sidekar bus wait <agent> [--until <state>] [--timeout <ms>]"
            );
        } else if target.is_none() {
            target = Some(arg);
        } else {
            bail!("Unexpected argument {arg}. Wait targets one agent at a time.");
        }
        i += 1;
    }

    let Some(target) = target else {
        bail!("Usage: sidekar bus wait <agent> [--until <state>] [--timeout <ms>]");
    };

    let agent = resolve_agent(state, target)?;
    let name = agent.id.name.clone();
    let label = agent
        .id
        .nick
        .clone()
        .map(|n| format!("{n} ({name})"))
        .unwrap_or_else(|| name.clone());

    if name == state.name().unwrap_or_default() {
        bail!("Cannot wait on yourself — \"{label}\" is this session.");
    }

    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    // Always assigned at the top of the loop before any exit from it.
    let mut last;
    let mut ever_fresh = false;

    loop {
        last = broker::get_agent_activity(&name)?.unwrap_or_else(ActivitySnapshot::unknown);
        ever_fresh |= !last.is_stale();

        if until.satisfied_by(last.state) && !last.is_stale() {
            out!(
                ctx,
                "{}",
                crate::output::to_string(&crate::output::PlainOutput::new(format!(
                    "{label} is {}.",
                    last.state.as_str()
                )))?
            );
            return Ok(());
        }

        if std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // Distinguish "watched it, it never got there" from "never saw it at all".
    // The second is not a slow agent, it is an agent nobody is reporting on —
    // usually one started without the PTY wrapper — and retrying will not help.
    if !ever_fresh {
        bail!(
            "No activity reported for {label} in {}s. \
             Activity is published by the sidekar PTY wrapper; an agent started \
             without it (plain `claude` rather than `sidekar claude`) never reports state.",
            timeout_ms / 1000
        );
    }
    bail!(
        "Timed out after {}s waiting for {label} to be {}. Last seen: {}.",
        timeout_ms / 1000,
        until.describe(),
        last.state.as_str()
    );
}

fn parse_timeout(v: &str) -> Result<u64> {
    let ms: u64 = v
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid --timeout={v}. Expected milliseconds."))?;
    if ms == 0 {
        bail!("Invalid --timeout=0. Expected a positive number of milliseconds.");
    }
    Ok(ms)
}

/// Find the agent by name or nickname, preferring this session's channel.
pub(super) fn resolve_agent(
    state: &crate::bus::SidekarBusState,
    target: &str,
) -> Result<crate::broker::BrokerAgent> {
    let want = crate::message::parse_target(target);
    let matches = |a: &crate::broker::BrokerAgent| {
        a.id.name == want
            || a.id.nick.as_deref() == Some(want.as_str())
            || a.id.name == target
            || a.id.nick.as_deref() == Some(target)
    };

    if let Some(channel) = state.channel()
        && let Some(found) = broker::list_agents(Some(channel))
            .unwrap_or_default()
            .into_iter()
            .find(matches)
    {
        return Ok(found);
    }
    // Fall back to every channel: waiting on an agent in a sibling worktree is
    // a normal thing to want, and unlike delivery it needs no transport.
    broker::list_agents(None)
        .unwrap_or_default()
        .into_iter()
        .find(matches)
        .ok_or_else(|| anyhow::anyhow!("No agent named \"{target}\". Try: sidekar bus who --all"))
}

#[cfg(test)]
mod tests;
