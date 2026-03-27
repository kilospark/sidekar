use crate::*;

pub async fn cmd_kv(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar kv <set|get|list|delete> [args...]");
    }
    match args[0].as_str() {
        "set" => cmd_kv_set(ctx, &args[1..]).await,
        "get" => cmd_kv_get(ctx, &args[1..]).await,
        "list" | "ls" => cmd_kv_list(ctx, &args[1..]).await,
        "delete" | "del" | "rm" => cmd_kv_delete(ctx, &args[1..]).await,
        _ => bail!("Unknown subcommand: {}. Use: set, get, list, delete", args[0]),
    }
}

fn get_agent_id() -> Option<String> {
    std::env::var("SIDEKAR_AGENT_NAME").ok()
}

async fn cmd_kv_set(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("Usage: sidekar kv set <key> <value> [--global]");
    }
    let key = &args[0];
    let value = &args[1];
    let global = args.iter().any(|a| a == "--global");

    let aid = get_agent_id();
    let agent_id = if global { None } else { aid.as_deref() };

    crate::broker::kv_set(agent_id, key, value)?;
    let scope = if global { "global" } else { "agent" };
    out!(ctx, "Set {} = {} ({})", key, value, scope);
    Ok(())
}

async fn cmd_kv_get(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar kv get <key> [--global]");
    }
    let key = &args[0];
    let global = args.iter().any(|a| a == "--global");

    let aid = get_agent_id();
    let agent_id = if global { None } else { aid.as_deref() };

    let entry = crate::broker::kv_get(agent_id, key)?
        .ok_or_else(|| anyhow::anyhow!("Key '{}' not found", key))?;

    out!(ctx, "{}", entry.value);
    Ok(())
}

async fn cmd_kv_list(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let global = args.iter().any(|a| a == "--global");

    let aid = get_agent_id();
    let agent_id = if global { None } else { aid.as_deref() };
    let entries = crate::broker::kv_list(agent_id)?;

    if entries.is_empty() {
        let scope = if global { "global" } else { "your agent" };
        out!(ctx, "No KV entries for {}.", scope);
        return Ok(());
    }

    let scope = if global { "global" } else { "agent" };
    out!(ctx, "KV entries ({}):", scope);
    for e in entries {
        out!(ctx, "  {} = {}", e.key, e.value);
    }
    Ok(())
}

async fn cmd_kv_delete(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar kv delete <key> [--global]");
    }
    let key = &args[0];
    let global = args.iter().any(|a| a == "--global");

    let aid = get_agent_id();
    let agent_id = if global { None } else { aid.as_deref() };

    crate::broker::kv_delete(agent_id, key)?;
    out!(ctx, "Deleted key '{}'.", key);
    Ok(())
}