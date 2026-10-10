//! Show why an agent is in the state the bus thinks it is in.
//!
//! Activity is inferred from someone else's TUI, so it is wrong sometimes: a
//! marker phrase appears in ordinary prose, an agent redraws without emitting
//! anything the detector recognises, a wrapper is missing entirely. When that
//! happens the visible symptom is indirect — a bus message that never lands, or
//! a `bus wait` that times out — and the state alone gives nothing to work
//! from.
//!
//! This prints the evidence behind the verdict: which rule fired and on what
//! line, how old the reading is, and whether anyone has looked at the agent
//! since it last finished. A wrong answer becomes a fixable one, usually by
//! editing `detect.*` (see `sidekar prompt`) rather than filing a bug.

use crate::activity::ACTIVITY_STALE_SECS;
use crate::app_context::AppContext;
use crate::broker;
use crate::message::epoch_secs;
use anyhow::{Result, bail};

/// `bus explain <agent>`.
pub fn cmd_explain(
    state: &crate::bus::SidekarBusState,
    ctx: &mut AppContext,
    args: &[String],
) -> Result<()> {
    let Some(target) = args.first().filter(|a| !a.starts_with('-')) else {
        bail!("Usage: sidekar bus explain <agent>");
    };

    let agent = match super::wait::resolve_agent(state, target) {
        Ok(agent) => agent,
        Err(not_here) => match broker::bus_sync::lookup_remote_agent(target)? {
            Some((_, remote)) => return explain_remote(ctx, &remote),
            None => return Err(not_here),
        },
    };
    let name = agent.id.name.clone();
    let label = match &agent.id.nick {
        Some(nick) => format!("{nick} ({name})"),
        None => name.clone(),
    };

    let Some(detail) = broker::get_agent_activity_detail(&name)? else {
        bail!("No activity row for {label}. It may have unregistered.");
    };
    let snapshot = detail.snapshot();
    let now = epoch_secs();

    let mut lines = vec![format!("{label} is {}", detail.state.as_str())];

    match &detail.reason {
        Some(reason) if !reason.is_empty() => lines.push(format!("  because: {reason}")),
        // Pre-4.5 wrappers wrote no reason, and neither does a plain
        // `update_agent_activity` from outside the PTY.
        _ => lines.push("  because: not recorded".to_string()),
    }

    if detail.at == 0 {
        lines.push("  reading:  never reported".to_string());
    } else {
        let age = now.saturating_sub(detail.at);
        let staleness = if snapshot.is_stale() {
            format!(" (stale — older than {ACTIVITY_STALE_SECS}s, treated as unknown)")
        } else {
            String::new()
        };
        lines.push(format!("  reading:  {}{staleness}", ago(age)));
    }

    match detail.settled_at {
        None => lines.push("  finished: not since it registered".to_string()),
        Some(settled) => {
            let seen = if detail.finished_unseen() {
                " — unseen"
            } else {
                " — seen"
            };
            lines.push(format!(
                "  finished: {}{seen}",
                ago(now.saturating_sub(settled))
            ));
        }
    }

    if snapshot.should_defer_delivery() {
        lines.push("  delivery: deferred — bus messages wait for this to clear".to_string());
    } else {
        lines.push("  delivery: open".to_string());
    }

    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(lines.join("\n")))?
    );
    Ok(())
}

/// `bus explain` for an agent on another of the account's machines: what its
/// presence last said, and how old that is.
fn explain_remote(ctx: &mut AppContext, agent: &broker::bus_sync::RemoteAgent) -> Result<()> {
    let label = agent.label();
    let now = epoch_secs();
    let published = now.saturating_sub(agent.published_at.max(0) as u64);
    let Some(activity) = &agent.activity else {
        bail!(
            "{label} publishes no activity: its machine runs a sidekar from before \
             cross-machine bus explain. Update sidekar there."
        );
    };
    let detail = activity.detail(now);
    let mut lines = vec![format!("{label} is {}", detail.state.as_str())];
    match &detail.reason {
        Some(reason) if !reason.is_empty() => lines.push(format!("  because: {reason}")),
        _ => lines.push("  because: not recorded".to_string()),
    }
    if activity.at == 0 {
        lines.push("  reading:  never reported".to_string());
    } else {
        let staleness = if activity.fresh {
            String::new()
        } else {
            format!(" (stale — older than {ACTIVITY_STALE_SECS}s there, treated as unknown)")
        };
        lines.push(format!(
            "  reading:  {} on its machine's clock{staleness}",
            ago(now.saturating_sub(activity.at))
        ));
    }
    lines.push(format!(
        "  synced:   presence published {}; changes arrive on that machine's next bus sync round",
        ago(published)
    ));
    match detail.settled_at {
        None => lines.push("  finished: not since it registered".to_string()),
        Some(settled) => {
            let seen = if detail.finished_unseen() {
                " — unseen"
            } else {
                " — seen"
            };
            lines.push(format!("  finished: {}{seen}", ago(now.saturating_sub(settled))));
        }
    }
    if agent.pending > 0 {
        lines.push(format!("  pending:  {} request(s) waiting on its answer", agent.pending));
    }
    if detail.snapshot().should_defer_delivery() {
        lines.push("  delivery: via sync; deferred there until this clears".to_string());
    } else {
        lines.push("  delivery: via sync, open".to_string());
    }
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(lines.join("\n")))?
    );
    Ok(())
}

/// Coarse relative time. Detection is second-granularity at best, so a
/// precise-looking figure would overstate what is known.
fn ago(secs: u64) -> String {
    match secs {
        0..=1 => "just now".to_string(),
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests;
