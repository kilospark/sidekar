use super::*;

pub(super) fn cmd_agent_sessions(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "" => cmd_agent_sessions_list(ctx, args),
        s if s.starts_with('-') => cmd_agent_sessions_list(ctx, args),
        "show" => cmd_agent_sessions_show(ctx, &args[1..]),
        "rename" => cmd_agent_sessions_rename(ctx, &args[1..]),
        "note" => cmd_agent_sessions_note(ctx, &args[1..]),
        other => bail!(
            "Usage: sidekar agent-sessions [show|rename|note] ... [--limit=N] [--active] [--project=<name>|--all-projects] (unknown subcommand: {other})"
        ),
    }
}

#[derive(serde::Serialize)]
struct AgentSessionListItem {
    id: String,
    name: Option<String>,
    agent: String,
    nick: Option<String>,
    project: String,
    channel: Option<String>,
    started_at: u64,
    last_active_at: u64,
    ended_at: Option<u64>,
    request_count: u64,
    reply_count: u64,
}

#[derive(serde::Serialize)]
struct AgentSessionsListOutput {
    items: Vec<AgentSessionListItem>,
}

impl crate::output::CommandOutput for AgentSessionsListOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "0 agent sessions.")?;
            return Ok(());
        }
        let active = self.items.iter().filter(|s| s.ended_at.is_none()).count();
        if active > 0 {
            writeln!(w, "{} sessions ({} active):", self.items.len(), active)?;
        } else {
            writeln!(w, "{} sessions:", self.items.len())?;
        }
        writeln!(
            w,
            "id\tname\tagent\tnick\tproject\tchannel\tstarted_at\tlast_active_at\tended_at\trequests\treplies"
        )?;
        for s in &self.items {
            writeln!(
                w,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                s.id,
                s.name.as_deref().unwrap_or("-"),
                s.agent,
                s.nick.as_deref().unwrap_or("-"),
                s.project,
                s.channel.as_deref().unwrap_or("-"),
                s.started_at,
                s.last_active_at,
                s.ended_at
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".into()),
                s.request_count,
                s.reply_count,
            )?;
        }
        Ok(())
    }
}

fn cmd_agent_sessions_list(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let active_only = args.iter().any(|a| a == "--active");
    let all_projects = args.iter().any(|a| a == "--all-projects");
    let project = if all_projects {
        None
    } else if let Some(explicit) = args.iter().find_map(|a| a.strip_prefix("--project=")) {
        Some(explicit.to_string())
    } else {
        Some(crate::scope::resolve_project_name(None))
    };
    let limit = args
        .iter()
        .find_map(|a| a.strip_prefix("--limit="))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20);

    let sessions = crate::broker::list_agent_sessions(active_only, project.as_deref(), limit)?;
    let output = AgentSessionsListOutput {
        items: sessions
            .into_iter()
            .map(|s| AgentSessionListItem {
                id: s.id,
                name: s.display_name,
                agent: s.agent_name,
                nick: s.nick,
                project: s.project,
                channel: s.channel,
                started_at: s.started_at,
                last_active_at: s.last_active_at,
                ended_at: s.ended_at,
                request_count: s.request_count,
                reply_count: s.reply_count,
            })
            .collect(),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

#[derive(serde::Serialize)]
struct AgentSessionShowOutput {
    id: String,
    display_name: Option<String>,
    agent_name: String,
    agent_type: Option<String>,
    nick: Option<String>,
    project: String,
    channel: Option<String>,
    cwd: Option<String>,
    started_at: u64,
    ended_at: Option<u64>,
    last_active_at: u64,
    request_count: u64,
    reply_count: u64,
    message_count: u64,
    last_request_msg_id: Option<String>,
    last_reply_msg_id: Option<String>,
    notes: Option<String>,
}

impl crate::output::CommandOutput for AgentSessionShowOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "id: {}", self.id)?;
        writeln!(
            w,
            "display_name: {}",
            self.display_name.as_deref().unwrap_or("-")
        )?;
        writeln!(w, "agent_name: {}", self.agent_name)?;
        writeln!(
            w,
            "agent_type: {}",
            self.agent_type.as_deref().unwrap_or("-")
        )?;
        writeln!(w, "nick: {}", self.nick.as_deref().unwrap_or("-"))?;
        writeln!(w, "project: {}", self.project)?;
        writeln!(w, "channel: {}", self.channel.as_deref().unwrap_or("-"))?;
        writeln!(w, "cwd: {}", self.cwd.as_deref().unwrap_or("-"))?;
        writeln!(w, "started_at: {}", self.started_at)?;
        writeln!(
            w,
            "ended_at: {}",
            self.ended_at
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".into())
        )?;
        writeln!(w, "last_active_at: {}", self.last_active_at)?;
        writeln!(w, "request_count: {}", self.request_count)?;
        writeln!(w, "reply_count: {}", self.reply_count)?;
        writeln!(w, "message_count: {}", self.message_count)?;
        writeln!(
            w,
            "last_request_msg_id: {}",
            self.last_request_msg_id.as_deref().unwrap_or("-")
        )?;
        writeln!(
            w,
            "last_reply_msg_id: {}",
            self.last_reply_msg_id.as_deref().unwrap_or("-")
        )?;
        writeln!(w, "notes: {}", self.notes.as_deref().unwrap_or("-"))?;
        Ok(())
    }
}

fn cmd_agent_sessions_show(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id = args.first().map(String::as_str).unwrap_or_default();
    if id.is_empty() {
        bail!("Usage: sidekar agent-sessions show <id>");
    }
    let s = crate::broker::get_agent_session(id)?
        .ok_or_else(|| anyhow!("Unknown agent session: {id}"))?;

    let output = AgentSessionShowOutput {
        id: s.id,
        display_name: s.display_name,
        agent_name: s.agent_name,
        agent_type: s.agent_type,
        nick: s.nick,
        project: s.project,
        channel: s.channel,
        cwd: s.cwd,
        started_at: s.started_at,
        ended_at: s.ended_at,
        last_active_at: s.last_active_at,
        request_count: s.request_count,
        reply_count: s.reply_count,
        message_count: s.message_count,
        last_request_msg_id: s.last_request_msg_id,
        last_reply_msg_id: s.last_reply_msg_id,
        notes: s.notes,
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

fn cmd_agent_sessions_rename(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id = args.first().map(String::as_str).unwrap_or_default();
    let display_name = args
        .get(1)
        .map(|_| args[1..].join(" "))
        .unwrap_or_default()
        .trim()
        .to_string();
    if id.is_empty() || display_name.is_empty() {
        bail!("Usage: sidekar agent-sessions rename <id> <display-name>");
    }
    if !crate::broker::set_agent_session_display_name(id, Some(display_name.as_str()))? {
        bail!("Unknown agent session: {id}");
    }
    let msg = format!("Renamed agent session {id} to \"{display_name}\".");
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(msg))?
    );
    Ok(())
}

fn cmd_agent_sessions_note(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id = args.first().map(String::as_str).unwrap_or_default();
    if id.is_empty() {
        bail!("Usage: sidekar agent-sessions note <id> <text> [--clear]");
    }
    let clear = args.iter().any(|a| a == "--clear");
    let notes = if clear {
        None
    } else {
        let text = args
            .iter()
            .skip(1)
            .filter(|a| a.as_str() != "--clear")
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string();
        if text.is_empty() {
            bail!("Usage: sidekar agent-sessions note <id> <text> [--clear]");
        }
        Some(text)
    };
    if !crate::broker::set_agent_session_notes(id, notes.as_deref())? {
        bail!("Unknown agent session: {id}");
    }
    let msg = if clear {
        format!("Cleared notes for agent session {id}.")
    } else {
        format!("Updated notes for agent session {id}.")
    };
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(msg))?
    );
    Ok(())
}
