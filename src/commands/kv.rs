use crate::*;

pub async fn cmd_kv(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar kv <set|get|list|delete|tag|history|rollback|exec> [args...]");
    }
    match args[0].as_str() {
        "set" => cmd_kv_set(ctx, &args[1..]).await,
        "get" => cmd_kv_get(ctx, &args[1..]).await,
        "list" | "ls" => cmd_kv_list(ctx, &args[1..]).await,
        "delete" | "del" | "rm" => cmd_kv_delete(ctx, &args[1..]).await,
        "tag" => cmd_kv_tag(ctx, &args[1..]).await,
        "history" => cmd_kv_history(ctx, &args[1..]).await,
        "rollback" => cmd_kv_rollback(ctx, &args[1..]).await,
        "exec" => cmd_kv_exec(ctx, &args[1..]).await,
        _ => bail!(
            "Unknown subcommand: {}. Use: set, get, list, delete, tag, history, rollback, exec",
            args[0]
        ),
    }
}

async fn cmd_kv_set(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    // Parse --tag=a,b or --tag a,b
    let mut tags: Option<Vec<String>> = None;
    let mut positional = Vec::new();

    let mut i = 0;
    while i < args.len() {
        if let Some(val) = args[i].strip_prefix("--tag=") {
            tags = Some(val.split(',').map(|s| s.trim().to_string()).collect());
        } else if args[i] == "--tag" {
            i += 1;
            if i < args.len() {
                tags = Some(args[i].split(',').map(|s| s.trim().to_string()).collect());
            }
        } else {
            positional.push(&args[i]);
        }
        i += 1;
    }

    if positional.len() < 2 {
        bail!("Usage: sidekar kv set <key> <value> [--tag=a,b]");
    }
    let key = positional[0];
    let value = positional[1];

    crate::broker::kv_set(key, value, tags.as_deref())?;
    let tag_str = tags
        .as_ref()
        .map(|t| format!(" [{}]", t.join(",")))
        .unwrap_or_default();
    out!(ctx, "Set {}{}", key, tag_str);
    Ok(())
}

async fn cmd_kv_get(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar kv get <key>");
    }
    let key = &args[0];

    let entry =
        crate::broker::kv_get(key)?.ok_or_else(|| anyhow::anyhow!("Key '{}' not found", key))?;

    if !entry.tags.is_empty() {
        out!(ctx, "{} [{}]", entry.value, entry.tags.join(","));
    } else {
        out!(ctx, "{}", entry.value);
    }
    Ok(())
}

async fn cmd_kv_list(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let filter_tag = args
        .iter()
        .find_map(|a| a.strip_prefix("--tag=").map(String::from))
        .or_else(|| {
            args.iter()
                .position(|a| a == "--tag")
                .and_then(|i| args.get(i + 1).cloned())
        });

    let json_output = args.iter().any(|a| a == "--json");
    let entries = crate::broker::kv_list(filter_tag.as_deref())?;

    if json_output {
        let items: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "key": e.key,
                    "value": e.value,
                    "tags": e.tags,
                })
            })
            .collect();
        out!(
            ctx,
            "{}",
            serde_json::to_string_pretty(&items).unwrap_or_default()
        );
        return Ok(());
    }

    if entries.is_empty() {
        out!(ctx, "No KV entries.");
        return Ok(());
    }

    for e in entries {
        let tag_str = if e.tags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", e.tags.join(","))
        };
        out!(ctx, "  {} = {}{}", e.key, e.value, tag_str);
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

async fn cmd_kv_tag(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.len() < 3 {
        bail!("Usage: sidekar kv tag <add|remove> <key> <tag1,tag2,...>");
    }
    let action = &args[0];
    let key = &args[1];
    let tag_list: Vec<String> = args[2].split(',').map(|s| s.trim().to_string()).collect();

    match action.as_str() {
        "add" => {
            crate::broker::kv_tag_add(key, &tag_list)?;
            out!(ctx, "Added tags [{}] to '{}'.", tag_list.join(","), key);
        }
        "remove" | "rm" => {
            crate::broker::kv_tag_remove(key, &tag_list)?;
            out!(ctx, "Removed tags [{}] from '{}'.", tag_list.join(","), key);
        }
        _ => bail!("Usage: sidekar kv tag <add|remove> <key> <tags>"),
    }
    Ok(())
}

async fn cmd_kv_history(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar kv history <key>");
    }
    let key = &args[0];
    let entries = crate::broker::kv_history(key)?;

    if entries.is_empty() {
        out!(ctx, "No history for '{}'.", key);
        return Ok(());
    }

    // Also show current value
    if let Ok(Some(current)) = crate::broker::kv_get(key) {
        let tag_str = if current.tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", current.tags.join(","))
        };
        out!(ctx, "  current  {}{}", current.value, tag_str);
    }

    for e in &entries {
        let tag_str = if e.tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", e.tags.join(","))
        };
        // Show relative time
        let now = crate::message::epoch_secs();
        let ago = now.saturating_sub(e.archived_at);
        let age = if ago < 60 {
            format!("{}s ago", ago)
        } else if ago < 3600 {
            format!("{}m ago", ago / 60)
        } else if ago < 86400 {
            format!("{}h ago", ago / 3600)
        } else {
            format!("{}d ago", ago / 86400)
        };
        out!(ctx, "  v{}  {}{}  ({})", e.version, e.value, tag_str, age);
    }
    Ok(())
}

async fn cmd_kv_rollback(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("Usage: sidekar kv rollback <key> <version>");
    }
    let key = &args[0];
    let version: i64 = args[1]
        .parse()
        .map_err(|_| anyhow!("Version must be a number"))?;

    crate::broker::kv_rollback(key, version)?;
    out!(ctx, "Rolled back '{}' to v{}.", key, version);
    Ok(())
}

async fn cmd_kv_exec(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    // Parse: kv exec [--keys=K1,K2] [--tag=TAG] <command> [args...]
    // Flags come first, then the command and its arguments.
    if args.is_empty() {
        bail!("Usage: sidekar kv exec [--keys=K1,K2] [--tag=TAG] <command> [args...]");
    }

    let mut keys = Vec::new();
    let mut filter_tag: Option<String> = None;
    let mut cmd_start = args.len(); // default: no command found

    let mut i = 0;
    while i < args.len() {
        if let Some(val) = args[i].strip_prefix("--keys=") {
            keys = val.split(',').map(|s| s.trim().to_string()).collect();
        } else if let Some(val) = args[i].strip_prefix("--tag=") {
            filter_tag = Some(val.to_string());
        } else if args[i] == "--tag" {
            i += 1;
            if i < args.len() {
                filter_tag = Some(args[i].clone());
            }
        } else if args[i] == "--keys" {
            i += 1;
            if i < args.len() {
                keys = args[i].split(',').map(|s| s.trim().to_string()).collect();
            }
        } else {
            // First non-flag arg is the command
            cmd_start = i;
            break;
        }
        i += 1;
    }

    let cmd_args = &args[cmd_start..];
    if cmd_args.is_empty() {
        bail!(
            "No command specified. Usage: sidekar kv exec [--keys=K1,K2] [--tag=TAG] <command> [args...]"
        );
    }

    let entries = crate::broker::kv_get_for_exec(&keys, filter_tag.as_deref())?;
    if entries.is_empty() {
        bail!("No matching KV entries to inject.");
    }

    // Build env map: key → decrypted value
    let mut secrets: Vec<(String, String)> = Vec::new();
    for e in &entries {
        secrets.push((e.key.clone(), e.value.clone()));
    }

    // Pass args as-is — secrets are injected only as env vars to avoid
    // leaking values in the process argv (visible via ps/proc).
    let program = &cmd_args[0];
    let program_args = &cmd_args[1..];

    // Spawn subprocess with secrets injected as env vars
    let mut cmd = std::process::Command::new(program);
    cmd.args(program_args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Inject secrets into environment
    for (k, v) in &secrets {
        cmd.env(k, v);
    }

    let child = cmd
        .spawn()
        .map_err(|e| anyhow!("Failed to spawn '{}': {}", program, e))?;
    let output = child
        .wait_with_output()
        .map_err(|e| anyhow!("Failed to wait for '{}': {}", program, e))?;

    // Collect secret values for masking
    let secret_values: Vec<&str> = secrets
        .iter()
        .filter(|(_, v)| v.len() >= 4) // skip very short values to avoid false masking
        .map(|(_, v)| v.as_str())
        .collect();

    // Output stdout with secrets masked
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.is_empty() {
        out!(ctx, "{}", mask_secrets(&stdout, &secret_values));
    }

    // Output stderr with secrets masked
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        out!(ctx, "{}", mask_secrets(&stderr, &secret_values));
    }

    if !output.status.success() {
        let code = output.status.code().unwrap_or(1);
        bail!("Command exited with status {}", code);
    }

    Ok(())
}

/// Replace all occurrences of secret values with [REDACTED].
fn mask_secrets(text: &str, secrets: &[&str]) -> String {
    let mut masked = text.to_string();
    for secret in secrets {
        masked = masked.replace(secret, "[REDACTED]");
    }
    masked
}
