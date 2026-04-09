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
    if sessions.is_empty() {
        out!(ctx, "0 agent sessions.");
        return Ok(());
    }

    let active = sessions.iter().filter(|s| s.ended_at.is_none()).count();
    if active > 0 {
        out!(ctx, "{} sessions ({} active):", sessions.len(), active);
    } else {
        out!(ctx, "{} sessions:", sessions.len());
    }
    out!(
        ctx,
        "id\tname\tagent\tnick\tproject\tchannel\tstarted_at\tlast_active_at\tended_at\trequests\treplies"
    );
    for session in sessions {
        out!(
            ctx,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            session.id,
            session.display_name.as_deref().unwrap_or("-"),
            session.agent_name,
            session.nick.as_deref().unwrap_or("-"),
            session.project,
            session.channel.as_deref().unwrap_or("-"),
            session.started_at,
            session.last_active_at,
            session
                .ended_at
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".into()),
            session.request_count,
            session.reply_count,
        );
    }
    Ok(())
}

fn cmd_agent_sessions_show(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id = args.first().map(String::as_str).unwrap_or_default();
    if id.is_empty() {
        bail!("Usage: sidekar agent-sessions show <id>");
    }
    let session = crate::broker::get_agent_session(id)?
        .ok_or_else(|| anyhow!("Unknown agent session: {id}"))?;

    out!(ctx, "id: {}", session.id);
    out!(
        ctx,
        "display_name: {}",
        session.display_name.as_deref().unwrap_or("-")
    );
    out!(ctx, "agent_name: {}", session.agent_name);
    out!(
        ctx,
        "agent_type: {}",
        session.agent_type.as_deref().unwrap_or("-")
    );
    out!(ctx, "nick: {}", session.nick.as_deref().unwrap_or("-"));
    out!(ctx, "project: {}", session.project);
    out!(
        ctx,
        "channel: {}",
        session.channel.as_deref().unwrap_or("-")
    );
    out!(ctx, "cwd: {}", session.cwd.as_deref().unwrap_or("-"));
    out!(ctx, "started_at: {}", session.started_at);
    out!(
        ctx,
        "ended_at: {}",
        session
            .ended_at
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into())
    );
    out!(ctx, "last_active_at: {}", session.last_active_at);
    out!(ctx, "request_count: {}", session.request_count);
    out!(ctx, "reply_count: {}", session.reply_count);
    out!(ctx, "message_count: {}", session.message_count);
    out!(
        ctx,
        "last_request_msg_id: {}",
        session.last_request_msg_id.as_deref().unwrap_or("-")
    );
    out!(
        ctx,
        "last_reply_msg_id: {}",
        session.last_reply_msg_id.as_deref().unwrap_or("-")
    );
    out!(ctx, "notes: {}", session.notes.as_deref().unwrap_or("-"));
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
    out!(ctx, "Renamed agent session {id} to \"{display_name}\".");
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
    if clear {
        out!(ctx, "Cleared notes for agent session {id}.");
    } else {
        out!(ctx, "Updated notes for agent session {id}.");
    }
    Ok(())
}
