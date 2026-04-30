use crate::memory::cmd_memory;
use crate::repo::cmd_repo;
use crate::rtk::cmd_compact;
use crate::tasks::cmd_tasks;
use crate::*;

use super::agent_sessions::cmd_agent_sessions;
use super::cron;
use super::doc::cmd_doc;
use super::journal::cmd_journal;
use super::kv::cmd_kv;
use super::monitor::cmd_monitor;
use super::totp::cmd_totp;
use crate::pakt::{cmd_pack, cmd_unpack};

pub(super) async fn dispatch_agent_command(
    ctx: &mut AppContext,
    command: &str,
    args: &[String],
) -> Option<Result<()>> {
    let result = match command {
        "monitor" => cmd_monitor(ctx, args).await,
        "memory" => cmd_memory(ctx, args).await,
        "journal" => cmd_journal(ctx, args),
        "tasks" => cmd_tasks(ctx, args),
        "agent-sessions" => cmd_agent_sessions(ctx, args),
        "repo" => cmd_repo(ctx, args),
        "compact" => cmd_compact(ctx, args),
        "bus" => dispatch_bus_root(ctx, args).await,
        "bus-who" => cmd_bus_who(ctx, args),
        "bus-requests" => cmd_bus_requests(ctx, args),
        "bus-replies" => cmd_bus_replies(ctx, args),
        "bus-show" => cmd_bus_show(ctx, args),
        "bus-send" => cmd_bus_send(ctx, args),
        "bus-done" => cmd_bus_done(ctx, args),
        "bus-cancel" => cmd_bus_cancel(ctx, args),
        "cron" => dispatch_cron_root(ctx, args).await,
        "cron-create" => cmd_cron_create(ctx, args).await,
        "cron-list" => cmd_cron_list(ctx, args).await,
        "cron-show" => cmd_cron_show(ctx, args).await,
        "cron-delete" => cmd_cron_delete(ctx, args).await,
        "loop" => cmd_loop(ctx, args).await,
        "pack" => cmd_pack(ctx, args),
        "unpack" => cmd_unpack(ctx, args),
        "totp" => cmd_totp(ctx, args).await,
        "kv" => cmd_kv(ctx, args).await,
        "doc" => cmd_doc(ctx, args),
        _ => return None,
    };
    Some(result)
}

async fn dispatch_bus_root(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let subcommand = match sub {
        "who" => "bus-who",
        "requests" => "bus-requests",
        "replies" => "bus-replies",
        "show" => "bus-show",
        "send" => "bus-send",
        "done" => "bus-done",
        "cancel" => "bus-cancel",
        _ => bail!("Usage: sidekar bus <who|requests|replies|show|send|done|cancel> [args...]"),
    };
    Box::pin(super::dispatch(ctx, subcommand, &args[1..])).await
}

fn cmd_bus_who(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let show_all = args.iter().any(|a| a == "--all" || a == "-a");
    let bus_state = recovered_bus_state(ctx);
    crate::bus::cmd_who(&bus_state, ctx, show_all)?;
    Ok(())
}

fn cmd_bus_requests(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let bus_state = recovered_bus_state(ctx);
    let status = args.iter().find_map(|a| a.strip_prefix("--status="));
    let status = match status {
        Some("all") | None => None,
        Some("open") => Some("open"),
        Some("answered") => Some("answered"),
        Some("timed-out") | Some("timed_out") => Some("timed_out"),
        Some("cancelled") => Some("cancelled"),
        Some(other) => {
            bail!("Invalid --status={other}. Valid: open, answered, timed-out, cancelled, all")
        }
    };
    let limit = args
        .iter()
        .find_map(|a| a.strip_prefix("--limit="))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20);
    crate::bus::cmd_requests(&bus_state, ctx, status, limit)?;
    Ok(())
}

fn cmd_bus_replies(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let bus_state = recovered_bus_state(ctx);
    let limit = args
        .iter()
        .find_map(|a| a.strip_prefix("--limit="))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20);
    let msg_id = args.iter().find_map(|a| a.strip_prefix("--msg-id="));
    crate::bus::cmd_replies(&bus_state, ctx, msg_id, limit)?;
    Ok(())
}

fn cmd_bus_show(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let bus_state = recovered_bus_state(ctx);
    let msg_id = args.first().map(String::as_str).unwrap_or_default();
    if msg_id.is_empty() {
        bail!("Usage: sidekar bus show <msg_id>");
    }
    crate::bus::cmd_show_request(&bus_state, ctx, msg_id)?;
    Ok(())
}

fn cmd_bus_send(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if ctx.agent_name.is_none() {
        eprintln!(
            "Warning: Not running inside sidekar wrapper. For full bus features, relaunch with: sidekar <agent-cli>"
        );
    }
    let reply_to = args.iter().find_map(|a| a.strip_prefix("--reply-to="));
    let kind = args
        .iter()
        .find_map(|a| a.strip_prefix("--kind="))
        .unwrap_or_else(|| {
            if reply_to.is_some() {
                "response"
            } else {
                "request"
            }
        });
    let file_path = args.iter().find_map(|a| a.strip_prefix("--file="));
    let filtered: Vec<&str> = args
        .iter()
        .filter(|a| {
            !a.starts_with("--kind=") && !a.starts_with("--reply-to=") && !a.starts_with("--file=")
        })
        .map(String::as_str)
        .collect();
    let to = filtered.first().copied().unwrap_or_default();
    let message = if let Some(path) = file_path {
        std::fs::read_to_string(path).with_context(|| format!("failed to read --file={path}"))?
    } else if filtered.len() > 1 {
        filtered[1..].join(" ")
    } else {
        String::new()
    };
    if to.is_empty() || message.is_empty() {
        bail!(
            "Usage: sidekar bus send <to> <message|--file=path> [--kind=request|fyi|response] [--reply-to=<msg_id>]"
        );
    }
    let mut bus_state = recovered_bus_state(ctx);
    crate::bus::cmd_send_message(&mut bus_state, ctx, to, &message, kind, reply_to)?;
    Ok(())
}

fn cmd_bus_done(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if ctx.agent_name.is_none() {
        eprintln!(
            "Warning: Not running inside sidekar wrapper. For full bus features, relaunch with: sidekar <agent-cli>"
        );
    }
    let reply_to = args.iter().find_map(|a| a.strip_prefix("--reply-to="));
    let file_path = args.iter().find_map(|a| a.strip_prefix("--file="));
    let filtered: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with("--reply-to=") && !a.starts_with("--file="))
        .map(String::as_str)
        .collect();
    if filtered.len() < 2 || (filtered.len() < 3 && file_path.is_none()) {
        bail!(
            "Usage: sidekar bus done <next> <summary> <request|--file=path> [--reply-to=<msg_id>]"
        );
    }
    let request_body = if let Some(path) = file_path {
        std::fs::read_to_string(path).with_context(|| format!("failed to read --file={path}"))?
    } else {
        filtered[2..].join(" ")
    };
    let mut bus_state = recovered_bus_state(ctx);
    crate::bus::cmd_signal_done(
        &mut bus_state,
        ctx,
        filtered[0],
        filtered[1],
        &request_body,
        reply_to,
    )?;
    Ok(())
}

fn cmd_bus_cancel(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    // `cancel` must be scoped to an existing agent identity — running it
    // outside a wrapper would auto-register a throwaway name that owns
    // zero rows, silently "succeeding" while the user's real outbound
    // requests remain untouched. Refuse hard here instead.
    if ctx.agent_name.is_none() {
        bail!(
            "sidekar bus cancel must run inside a sidekar wrapper so it can scope to your agent identity. \
             Relaunch your agent with: sidekar <agent-cli>"
        );
    }
    let all = args.iter().any(|a| a == "--all" || a == "-a");
    let msg_ids: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with("--") && a.as_str() != "-a")
        .map(String::as_str)
        .collect();
    if !all && msg_ids.is_empty() {
        bail!("Usage: sidekar bus cancel <msg_id>... | --all");
    }
    let bus_state = recovered_bus_state(ctx);
    crate::bus::cmd_cancel_request(&bus_state, ctx, &msg_ids, all)?;
    Ok(())
}

async fn dispatch_cron_root(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let subcommand = match sub {
        "create" => "cron-create",
        "list" => "cron-list",
        "show" => "cron-show",
        "delete" => "cron-delete",
        _ => bail!("Usage: sidekar cron <create|list|show|delete> [args...]"),
    };
    Box::pin(super::dispatch(ctx, subcommand, &args[1..])).await
}

async fn cmd_cron_create(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!(
            "Usage: sidekar cron create <schedule> <action_json|--bash=CMD|--prompt=TEXT> [--target=T] [--name=N]"
        );
    }
    let schedule = &args[0];
    let bash_val = args.iter().find_map(|a| a.strip_prefix("--bash="));
    let prompt_val = args.iter().find_map(|a| a.strip_prefix("--prompt="));

    let action: serde_json::Value = if let Some(cmd) = bash_val {
        json!({"command": cmd})
    } else if let Some(p) = prompt_val {
        json!({"prompt": p})
    } else {
        if args.len() < 2 {
            bail!(
                "Usage: sidekar cron create <schedule> <action_json|--bash=CMD|--prompt=TEXT> [--target=T] [--name=N]"
            );
        }
        serde_json::from_str(&args[1]).context(
            "Invalid action JSON. Use: {\"tool\":\"screenshot\"}, {\"command\":\"...\"}, {\"prompt\":\"...\"}, or --bash=CMD / --prompt=TEXT",
        )?
    };
    let target = args
        .iter()
        .find_map(|a| a.strip_prefix("--target="))
        .unwrap_or("self");
    let name = args.iter().find_map(|a| a.strip_prefix("--name="));
    let once = args.iter().any(|a| a == "--once");
    let scope = args.iter().find_map(|a| a.strip_prefix("--scope="));
    let project_name = crate::scope::resolve_project_name(None);
    let project = match scope {
        Some("global") => Some("global"),
        _ => Some(project_name.as_str()),
    };
    let created_by = ctx.agent_name.clone().unwrap_or_else(|| "cli".into());
    let id = cron::cmd_cron_create(
        ctx,
        schedule,
        &action,
        target,
        name,
        &created_by,
        once,
        project,
        None,
    )
    .await?;
    let _ = id;
    Ok(())
}

async fn cmd_cron_list(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let scope = args.iter().find_map(|a| a.strip_prefix("--scope="));
    let scope = crate::scope::ScopeView::parse(scope)?;
    cron::cmd_cron_list(ctx, scope).await
}

async fn cmd_cron_show(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id = args.first().map(String::as_str).unwrap_or_default();
    if id.is_empty() {
        bail!("Usage: sidekar cron show <job-id>");
    }
    cron::cmd_cron_show(ctx, id).await
}

async fn cmd_cron_delete(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id = args.first().map(String::as_str).unwrap_or_default();
    if id.is_empty() {
        bail!("Usage: sidekar cron delete <job-id>");
    }
    cron::cmd_cron_delete(ctx, id).await
}

async fn cmd_loop(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!(
            "Usage: sidekar loop <interval> <prompt> [--once]\n  e.g. sidekar loop 5m \"check deployment status\""
        );
    }
    let interval = &args[0];
    let once = args.iter().any(|a| a == "--once");
    let prompt_text = args[1..]
        .iter()
        .filter(|a| *a != "--once")
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let interval_secs = cron::interval_to_secs(interval)?;
    let schedule = "* * * * *";
    let action = json!({"prompt": prompt_text});
    let name_str = format!("loop-{interval}");
    let created_by = ctx.agent_name.clone().unwrap_or_else(|| "cli".into());
    let loop_project = crate::scope::resolve_project_name(None);
    let id = cron::cmd_cron_create(
        ctx,
        schedule,
        &action,
        "self",
        Some(&name_str),
        &created_by,
        once,
        Some(&loop_project),
        Some(interval_secs),
    )
    .await?;
    let _ = id;
    Ok(())
}

fn recovered_bus_state(ctx: &AppContext) -> crate::bus::SidekarBusState {
    let mut state = crate::bus::SidekarBusState::new();
    if let Some(name) = ctx.agent_name.as_deref() {
        if let Ok(Some(agent)) = crate::broker::find_agent(name, None) {
            state.identity = Some(agent.id);
            state.pane_unique_id = agent.pane_unique_id;
            state.inherited_pty = true;
            state.borrowed = true;
            return state;
        }
        state.borrowed = true;
        return state;
    }
    state.do_register(None);
    state
}
