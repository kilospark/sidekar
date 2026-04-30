use crate::*;

pub async fn cmd_kv(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        return cmd_kv_list(ctx, args).await;
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
    let msg = format!("Set {}{}", key, tag_str);
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(msg))?
    );
    Ok(())
}

async fn cmd_kv_get(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar kv get <key>");
    }
    let key = &args[0];

    let entry =
        crate::broker::kv_get(key)?.ok_or_else(|| anyhow::anyhow!("Key '{}' not found", key))?;

    let text = if !entry.tags.is_empty() {
        format!("{} [{}]", entry.value, entry.tags.join(","))
    } else {
        entry.value.clone()
    };
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(text))?
    );
    Ok(())
}

#[derive(serde::Serialize)]
struct KvEntryOut {
    key: String,
    value: String,
    tags: Vec<String>,
}

#[derive(serde::Serialize)]
struct KvListOutput {
    items: Vec<KvEntryOut>,
}

impl crate::output::CommandOutput for KvListOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "0 KV entries.")?;
            return Ok(());
        }
        let tagged = self.items.iter().filter(|e| !e.tags.is_empty()).count();
        if tagged > 0 {
            writeln!(w, "{} entries ({} tagged):", self.items.len(), tagged)?;
        } else {
            writeln!(w, "{} entries:", self.items.len())?;
        }
        for e in &self.items {
            if e.tags.is_empty() {
                writeln!(w, "  {} = {}", e.key, e.value)?;
            } else {
                writeln!(w, "  {} = {}  [{}]", e.key, e.value, e.tags.join(","))?;
            }
        }
        Ok(())
    }
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

    let entries = crate::broker::kv_list(filter_tag.as_deref())?;
    let output = KvListOutput {
        items: entries
            .into_iter()
            .map(|e| KvEntryOut {
                key: e.key,
                value: e.value,
                tags: e.tags,
            })
            .collect(),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

async fn cmd_kv_delete(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar kv delete <key>");
    }
    let key = &args[0];

    crate::broker::kv_delete(key)?;
    let msg = format!("Deleted key '{}'.", key);
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(msg))?
    );
    Ok(())
}

async fn cmd_kv_tag(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.len() < 3 {
        bail!("Usage: sidekar kv tag <add|remove> <key> <tag1,tag2,...>");
    }
    let action = &args[0];
    let key = &args[1];
    let tag_list: Vec<String> = args[2].split(',').map(|s| s.trim().to_string()).collect();

    let msg = match action.as_str() {
        "add" => {
            crate::broker::kv_tag_add(key, &tag_list)?;
            format!("Added tags [{}] to '{}'.", tag_list.join(","), key)
        }
        "remove" | "rm" => {
            crate::broker::kv_tag_remove(key, &tag_list)?;
            format!("Removed tags [{}] from '{}'.", tag_list.join(","), key)
        }
        _ => bail!("Usage: sidekar kv tag <add|remove> <key> <tags>"),
    };
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(msg))?
    );
    Ok(())
}

#[derive(serde::Serialize)]
struct KvHistoryEntryOut {
    version: String,
    value: String,
    tags: Vec<String>,
    age: Option<String>,
}

#[derive(serde::Serialize)]
struct KvHistoryOutput {
    key: String,
    versions: Vec<KvHistoryEntryOut>,
}

impl crate::output::CommandOutput for KvHistoryOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        // Count only the archived versions (those with an age); current is extra.
        let archived = self.versions.iter().filter(|v| v.age.is_some()).count();
        if archived == 0 {
            writeln!(w, "0 history entries for '{}'.", self.key)?;
            return Ok(());
        }
        writeln!(w, "{} versions for '{}':", archived, self.key)?;
        for e in &self.versions {
            let tag_str = if e.tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", e.tags.join(","))
            };
            match &e.age {
                Some(age) => writeln!(
                    w,
                    "  {}  {}{}  ({})",
                    e.version, e.value, tag_str, age
                )?,
                None => writeln!(w, "  {}  {}{}", e.version, e.value, tag_str)?,
            }
        }
        Ok(())
    }
}

async fn cmd_kv_history(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar kv history <key>");
    }
    let key = &args[0];
    let entries = crate::broker::kv_history(key)?;

    let mut versions: Vec<KvHistoryEntryOut> = Vec::new();

    if !entries.is_empty() {
        if let Ok(Some(current)) = crate::broker::kv_get(key) {
            versions.push(KvHistoryEntryOut {
                version: "current".to_string(),
                value: current.value,
                tags: current.tags,
                age: None,
            });
        }
        let now = crate::message::epoch_secs();
        for e in &entries {
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
            versions.push(KvHistoryEntryOut {
                version: format!("v{}", e.version),
                value: e.value.clone(),
                tags: e.tags.clone(),
                age: Some(age),
            });
        }
    }

    let output = KvHistoryOutput {
        key: key.to_string(),
        versions,
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
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
    let msg = format!("Rolled back '{}' to v{}.", key, version);
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(msg))?
    );
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
        let masked = mask_secrets(&stdout, &secret_values);
        out!(
            ctx,
            "{}",
            crate::output::to_string(&crate::output::PlainOutput::new(masked))?
        );
    }

    // Output stderr with secrets masked
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        let masked = mask_secrets(&stderr, &secret_values);
        out!(
            ctx,
            "{}",
            crate::output::to_string(&crate::output::PlainOutput::new(masked))?
        );
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
