//! `sidekar agents` — every agent on this machine, ordered by what needs you.
//!
//! `bus who` answers "who is on which channel". This answers the question a
//! person running several agents actually has: which of them is waiting on me?
//! An agent parked on a question comes first, then one that finished a turn
//! nobody has looked at, then the ones still working. The idle and the dead
//! come last, because they need nothing.
//!
//! This only became possible to show honestly once idle detection worked.
//! Before, an idle Claude Code read as working forever — it repaints its screen
//! five times a second — so "working" and "done" were the same row.

use crate::AppContext;
use crate::activity::{ACTIVITY_STALE_SECS, ActivityState};
use crate::broker::{ActivityDetail, BrokerAgent};
use anyhow::Result;
use serde::Serialize;
use std::io::Write;

/// How much an agent needs a human, most urgent first.
///
/// The derive order is the sort order, so the enum reads top to bottom as the
/// view does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Attention {
    /// Parked on a question only a human can answer.
    NeedsInput,
    /// Finished a turn that nobody has looked at since.
    DoneUnseen,
    Working,
    /// A human is typing into it right now.
    Typing,
    Idle,
    /// Still registered but no longer reporting; possibly hung.
    Stale,
    /// The process is gone and only its registration is left.
    Dead,
}

impl Attention {
    fn label(self) -> &'static str {
        match self {
            Self::NeedsInput => "needs input",
            Self::DoneUnseen => "done",
            Self::Working => "working",
            Self::Typing => "typing",
            Self::Idle => "idle",
            Self::Stale => "stale",
            Self::Dead => "dead",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Row {
    pub name: String,
    pub nick: String,
    /// The CLI being run: `claude`, `codex`, `repl`.
    pub harness: String,
    /// Last path component of the channel, e.g. `sidekar`.
    pub channel: String,
    pub attention: Attention,
    pub detail: String,
    pub spawned_by: Option<String>,
    /// This row is the agent running the command.
    pub you: bool,
}

/// Decide what an agent needs, from what the registry says about it.
///
/// Pure, so the rules can be tested without a broker. `alive` is whether its
/// process still exists; `detail` is its last published activity.
pub(crate) fn classify(
    alive: bool,
    detail: Option<&ActivityDetail>,
    now: u64,
) -> (Attention, String) {
    if !alive {
        return (Attention::Dead, "process gone".into());
    }
    let Some(d) = detail else {
        return (Attention::Stale, "never reported".into());
    };
    // Only a state that is supposed to keep refreshing can go stale. Working,
    // needs-input and typing are republished while they last, so a long silence
    // from one of those means the wrapper stopped. Idle is published once and
    // then left alone — its timestamp freezes the moment the agent goes quiet —
    // so an old idle reading is simply an agent that has been idle a while.
    let silent = now.saturating_sub(d.at);
    let refreshes = matches!(
        d.state,
        ActivityState::AgentWorking | ActivityState::NeedsInput | ActivityState::UserTyping
    );
    if refreshes && silent > ACTIVITY_STALE_SECS {
        return (Attention::Stale, format!("no report for {}", ago(silent)));
    }
    let reason = d.reason.clone().unwrap_or_default();
    match d.state {
        ActivityState::NeedsInput => (Attention::NeedsInput, reason),
        ActivityState::AgentWorking => (Attention::Working, reason),
        ActivityState::UserTyping => (Attention::Typing, "someone is typing".into()),
        ActivityState::Idle if d.finished_unseen() => {
            let since = d.settled_at.map(|s| now.saturating_sub(s)).unwrap_or(0);
            (
                Attention::DoneUnseen,
                format!("finished {} ago, not looked at", ago(since)),
            )
        }
        ActivityState::Idle => {
            let since = d.settled_at.map(|s| now.saturating_sub(s));
            (
                Attention::Idle,
                since
                    .map(|s| format!("idle {}", ago(s)))
                    .unwrap_or_else(|| "idle".into()),
            )
        }
        ActivityState::Unknown => (Attention::Stale, "state unknown".into()),
    }
}

/// The CLI an agent runs, read back from its registered name.
///
/// PTY names are `{agent}-{channel}-{n}` with the channel a path, so the agent
/// is everything before the first `-/`. Splitting on the first `-` alone would
/// turn `cursor-agent` into `cursor`.
pub(crate) fn harness_of(name: &str, agent_type: Option<&str>) -> String {
    if agent_type == Some("sidekar-repl") {
        return "repl".into();
    }
    match name.split_once("-/") {
        Some((agent, _)) => agent.to_string(),
        None => name.split('-').next().unwrap_or(name).to_string(),
    }
}

/// The last component of a channel path; the whole thing when it has none.
pub(crate) fn short_channel(channel: &str) -> String {
    channel
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(channel)
        .to_string()
}

/// "40s", "3m", "2h", "5d".
pub(crate) fn ago(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

#[derive(Serialize)]
struct AgentsOutput {
    agents: Vec<Row>,
}

impl crate::output::CommandOutput for AgentsOutput {
    fn render_text(&self, w: &mut dyn Write) -> std::io::Result<()> {
        write!(w, "{}", render(&self.agents))
    }
}

/// The table, most urgent first, with a one-line tally on top.
pub(crate) fn render(rows: &[Row]) -> String {
    if rows.is_empty() {
        return "No agents running.\n".into();
    }
    let mut rows: Vec<&Row> = rows.iter().collect();
    rows.sort_by(|a, b| a.attention.cmp(&b.attention).then(a.nick.cmp(&b.nick)));

    let mut out = String::new();
    out.push_str(&tally(&rows));
    out.push('\n');

    let nick_w = rows
        .iter()
        .map(|r| shown_nick(r).chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    let state_w = rows
        .iter()
        .map(|r| r.attention.label().len())
        .max()
        .unwrap_or(5)
        .max(5);
    let harness_w = rows
        .iter()
        .map(|r| r.harness.len())
        .max()
        .unwrap_or(5)
        .max(5);
    let chan_w = rows
        .iter()
        .map(|r| r.channel.len())
        .max()
        .unwrap_or(7)
        .max(7);

    for r in rows {
        let nick = shown_nick(r);
        let mut detail = r.detail.clone();
        if let Some(by) = &r.spawned_by {
            detail = if detail.is_empty() {
                format!("spawned by {by}")
            } else {
                format!("{detail} · spawned by {by}")
            };
        }
        out.push_str(&format!(
            "  {nick:<nick_w$}  {state:<state_w$}  {harness:<harness_w$}  {chan:<chan_w$}  {detail}\n",
            state = r.attention.label(),
            harness = r.harness,
            chan = r.channel,
        ));
    }
    out
}

/// Longest nick shown before it is cut. A real nick is one word; anything
/// longer is a bug upstream, and it should look like one rather than push every
/// other column off the screen.
const NICK_MAX: usize = 24;

fn shown_nick(r: &Row) -> String {
    let mut nick: String = r.nick.chars().take(NICK_MAX).collect();
    if r.nick.chars().count() > NICK_MAX {
        nick.push('…');
    }
    if r.you {
        nick.push_str(" (you)");
    }
    nick
}

/// "3 agents: 1 needs you, 1 done, 1 working".
fn tally(rows: &[&Row]) -> String {
    let count = |a: Attention| rows.iter().filter(|r| r.attention == a).count();
    let mut parts = Vec::new();
    let needs = count(Attention::NeedsInput);
    if needs > 0 {
        parts.push(format!("{needs} needs you"));
    }
    for a in [
        Attention::DoneUnseen,
        Attention::Working,
        Attention::Typing,
        Attention::Idle,
        Attention::Stale,
        Attention::Dead,
    ] {
        let n = count(a);
        if n > 0 {
            parts.push(format!("{n} {}", a.label()));
        }
    }
    let total = rows.len();
    format!(
        "{total} agent{}: {}",
        if total == 1 { "" } else { "s" },
        parts.join(", ")
    )
}

/// Gather one row per registered agent from the live registry.
fn gather() -> Result<Vec<Row>> {
    let now = crate::message::epoch_secs();
    let me = crate::runtime::agent_name();
    let spawned: std::collections::HashMap<String, String> = crate::broker::spawned_agents()
        .unwrap_or_default()
        .into_iter()
        .map(|(a, by)| (a.id.name, by))
        .collect();

    let agents = crate::broker::list_agents(None)?;
    // Spawners are recorded by bus name; people know them by nick.
    let nicks: std::collections::HashMap<String, String> = agents
        .iter()
        .filter_map(|a| a.id.nick.clone().map(|n| (a.id.name.clone(), n)))
        .collect();
    let mut rows = Vec::new();
    for a in &agents {
        let mut row = row_for(a, &spawned, me.as_deref(), now);
        if let Some(by) = row.spawned_by.take() {
            row.spawned_by = Some(nicks.get(&by).cloned().unwrap_or(by));
        }
        rows.push(row);
    }
    Ok(rows)
}

fn row_for(
    a: &BrokerAgent,
    spawned: &std::collections::HashMap<String, String>,
    me: Option<&str>,
    now: u64,
) -> Row {
    let alive =
        a.id.pane
            .as_deref()
            .and_then(crate::bus::presence::pid_of_pane)
            .map(crate::bus::presence::process_alive)
            // A pane we cannot map to a process might still be alive; only call it
            // dead when we actually looked and found nothing.
            .unwrap_or(true);
    let detail = crate::broker::get_agent_activity_detail(&a.id.name)
        .ok()
        .flatten();
    let (attention, detail_text) = classify(alive, detail.as_ref(), now);
    Row {
        name: a.id.name.clone(),
        nick: a.id.nick.clone().unwrap_or_else(|| a.id.name.clone()),
        harness: harness_of(&a.id.name, a.id.agent_type.as_deref()),
        channel: a
            .id
            .session
            .as_deref()
            .map(short_channel)
            .unwrap_or_default(),
        attention,
        detail: detail_text,
        spawned_by: spawned.get(&a.id.name).cloned().filter(|s| !s.is_empty()),
        you: me == Some(a.id.name.as_str()),
    }
}

pub async fn cmd_agents(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let watch = watch_interval(args)?;
    let Some(every) = watch else {
        let output = AgentsOutput { agents: gather()? };
        out!(ctx, "{}", crate::output::to_string(&output)?);
        return Ok(());
    };

    // Watch mode prints directly: `out!` buffers until the command returns, and
    // this one does not return until interrupted.
    let footer = format!(
        "\n  refreshing every {}s · Ctrl-C to stop\n",
        every.as_secs()
    );
    loop {
        let frame = format!("{}{footer}", render(&gather()?));
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(redraw(&frame).as_bytes());
        let _ = stdout.flush();
        tokio::time::sleep(every).await;
    }
}

/// Repaint in place without clearing the screen first.
///
/// Clearing then drawing shows an empty screen for a moment on every refresh.
/// Instead: go home, overwrite each line and clear only what is left of it, then
/// clear whatever is below the last line — so a shorter frame leaves no ghost
/// rows from a longer one.
pub(crate) fn redraw(frame: &str) -> String {
    let mut out = String::from("\x1b[H");
    for line in frame.split_inclusive('\n') {
        let text = line.trim_end_matches('\n');
        out.push_str(text);
        out.push_str("\x1b[K");
        if line.ends_with('\n') {
            out.push_str("\r\n");
        }
    }
    out.push_str("\x1b[J");
    out
}

/// `--watch` alone means every 2s; `--watch 5` or `--watch=5` sets it.
fn watch_interval(args: &[String]) -> Result<Option<std::time::Duration>> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(v) = a.strip_prefix("--watch=") {
            return Ok(Some(parse_secs(v)?));
        }
        if a == "--watch" || a == "-w" {
            return Ok(Some(match args.get(i + 1) {
                Some(v) if !v.starts_with('-') => parse_secs(v)?,
                _ => std::time::Duration::from_secs(2),
            }));
        }
        if a.starts_with('-') {
            anyhow::bail!("unknown flag {a}. Usage: sidekar agents [--watch [secs]]");
        }
        i += 1;
    }
    Ok(None)
}

fn parse_secs(v: &str) -> Result<std::time::Duration> {
    let n: u64 = v
        .parse()
        .map_err(|_| anyhow::anyhow!("--watch takes whole seconds, got {v:?}"))?;
    if n == 0 {
        anyhow::bail!("--watch 0 would redraw continuously; use 1 or more");
    }
    Ok(std::time::Duration::from_secs(n))
}

#[cfg(test)]
mod tests;
