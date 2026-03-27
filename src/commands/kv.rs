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

fn get_project_id() -> Option<String> {
    std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string())
}

async fn cmd_kv_set(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("Usage: sidekar kv set <key> <value> [--global]");
    }
    let key = &args[0];
    let value = &args[1];
    let global = args.iter().any(|a| a == "--global");

    let project = get_project_id();
    let scope_id = if global { None } else { project.as_deref() };

    crate::broker::kv_set(scope_id, key, value)?;
    let scope = if global { "global" } else { "project" };
    out!(ctx, "Set {} = {} ({})", key, value, scope);
    Ok(())
}

async fn cmd_kv_get(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar kv get <key> [--global]");
    }
    let key = &args[0];
    let global = args.iter().any(|a| a == "--global");

    let project = get_project_id();
    let scope_id = if global { None } else { project.as_deref() };

    let entry = crate::broker::kv_get(scope_id, key)?
        .ok_or_else(|| anyhow::anyhow!("Key '{}' not found", key))?;

    out!(ctx, "{}", entry.value);
    Ok(())
}

async fn cmd_kv_list(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let global = args.iter().any(|a| a == "--global");

    let project = get_project_id();
    let scope_id = if global { None } else { project.as_deref() };
    let entries = crate::broker::kv_list(scope_id)?;

    if entries.is_empty() {
        let scope = if global { "global" } else { "project" };
        out!(ctx, "No KV entries for {}.", scope);
        return Ok(());
    }

    let scope = if global { "global" } else { "project" };
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

    let project = get_project_id();
    let scope_id = if global { None } else { project.as_deref() };

    crate::broker::kv_delete(scope_id, key)?;
    out!(ctx, "Deleted key '{}'.", key);
    Ok(())
}