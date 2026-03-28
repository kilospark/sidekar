use crate::*;

pub async fn cmd_kv(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar kv <set|get|list|delete> [args...]");
    }
    match args[0].as_str() {
        "set" => cmd_kv_set(ctx, &args[1..]).await,
        "get" => cmd_kv_get(ctx, &args[1..]).await,
        "list" | "ls" => cmd_kv_list(ctx).await,
        "delete" | "del" | "rm" => cmd_kv_delete(ctx, &args[1..]).await,
        _ => bail!("Unknown subcommand: {}. Use: set, get, list, delete", args[0]),
    }
}

async fn cmd_kv_set(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("Usage: sidekar kv set <key> <value>");
    }
    let key = &args[0];
    let value = &args[1];

    crate::broker::kv_set(key, value)?;
    out!(ctx, "Set {} = {}", key, value);
    Ok(())
}

async fn cmd_kv_get(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar kv get <key>");
    }
    let key = &args[0];

    let entry = crate::broker::kv_get(key)?
        .ok_or_else(|| anyhow::anyhow!("Key '{}' not found", key))?;

    out!(ctx, "{}", entry.value);
    Ok(())
}

async fn cmd_kv_list(ctx: &mut AppContext) -> Result<()> {
    let entries = crate::broker::kv_list()?;

    if entries.is_empty() {
        out!(ctx, "No KV entries.");
        return Ok(());
    }

    out!(ctx, "KV entries:");
    for e in entries {
        out!(ctx, "  {} = {}", e.key, e.value);
    }
    Ok(())
}

async fn cmd_kv_delete(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar kv delete <key>");
    }
    let key = &args[0];

    crate::broker::kv_delete(key)?;
    out!(ctx, "Deleted key '{}'.", key);
    Ok(())
}