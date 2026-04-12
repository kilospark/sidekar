use crate::*;

use super::core::{cmd_setup, cmd_uninstall};

pub(super) async fn dispatch_system_command(
    ctx: &mut AppContext,
    command: &str,
    args: &[String],
) -> Option<Result<()>> {
    let result = match command {
        "event" => cmd_event(ctx, args),
        "install" => cmd_setup(ctx).await,
        "uninstall" => cmd_uninstall(ctx).await,
        "config" => cmd_config(ctx, args),
        "update" => cmd_update(ctx).await,
        "proxy" => cmd_proxy(ctx, args),
        _ => return None,
    };
    Some(result)
}

fn cmd_event(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" => {
            let options = parse_event_list_options(args)?;
            let rows = crate::broker::events_recent(options.limit, options.level_filter)?;

            if options.json_output {
                let items: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "id": r.id,
                            "created_at": r.created_at,
                            "level": r.level,
                            "source": r.source,
                            "message": r.message,
                            "details": r.details,
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

            if rows.is_empty() {
                out!(ctx, "No events.");
                return Ok(());
            }
            out!(ctx, "id\tcreated_at\tlevel\tsource\tmessage");
            for r in rows {
                let details = r.details.as_deref().unwrap_or("");
                let msg = if details.is_empty() {
                    r.message.clone()
                } else {
                    format!("{} | {}", r.message, details)
                };
                out!(
                    ctx,
                    "{}\t{}\t{}\t{}\t{}",
                    r.id,
                    r.created_at,
                    r.level,
                    r.source,
                    msg
                );
            }
            Ok(())
        }
        "clear" => {
            let level_filter = parse_event_clear_level(args)?;
            let deleted = crate::broker::events_clear(level_filter)?;
            out!(ctx, "Deleted {deleted} events.");
            Ok(())
        }
        _ => bail!("Unknown subcommand: event {sub}. Use: event list, event clear"),
    }
}

#[derive(Debug)]
struct EventListOptions<'a> {
    limit: usize,
    level_filter: Option<&'a str>,
    json_output: bool,
}

fn parse_event_list_options(args: &[String]) -> Result<EventListOptions<'_>> {
    let mut options = EventListOptions {
        limit: 50,
        level_filter: None,
        json_output: false,
    };
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => options.json_output = true,
            "--limit" => {
                let value = iter.next().context(
                    "Usage: sidekar event list [--level=error|debug|info] [N|--limit=N]",
                )?;
                options.limit = parse_event_limit(value)?;
            }
            _ if arg.starts_with("--limit=") => {
                let value = arg.trim_start_matches("--limit=");
                options.limit = parse_event_limit(value)?;
            }
            _ if arg.starts_with("--level=") => {
                let level = arg.trim_start_matches("--level=");
                options.level_filter = Some(parse_event_level(level)?);
            }
            _ if arg.starts_with("--") => bail!("Unknown option for event list: {arg}"),
            _ => options.limit = parse_event_limit(arg)?,
        }
    }
    Ok(options)
}

fn parse_event_clear_level(args: &[String]) -> Result<Option<&str>> {
    let mut level_filter = None;
    for arg in args.iter().skip(1) {
        if let Some(level) = arg.strip_prefix("--level=") {
            level_filter = Some(parse_event_level(level)?);
        } else {
            bail!("Unknown option for event clear: {arg}");
        }
    }
    Ok(level_filter)
}

fn parse_event_limit(value: &str) -> Result<usize> {
    let limit = value
        .parse::<usize>()
        .with_context(|| format!("Invalid event limit: {value}"))?;
    if limit == 0 {
        bail!("Invalid event limit: {value}");
    }
    Ok(limit)
}

fn parse_event_level(level: &str) -> Result<&str> {
    match level {
        "debug" | "info" | "error" => Ok(level),
        _ => bail!("Invalid event level: {level}. Use one of: debug, info, error"),
    }
}

fn cmd_config(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("list");
    match action {
        "list" | "ls" => {
            let items = crate::config::config_list();
            let max_key = items.iter().map(|(k, _, _)| k.len()).max().unwrap_or(0);
            for (key, val, is_default) in &items {
                let display_val = if val.is_empty() {
                    "(not set)"
                } else {
                    val.as_str()
                };
                let marker = if *is_default { " (default)" } else { "" };
                let desc = crate::config::find_key(key)
                    .map(|k| k.description)
                    .unwrap_or("");
                out!(
                    ctx,
                    "{:<width$}  {}{}",
                    key,
                    display_val,
                    marker,
                    width = max_key
                );
                if !desc.is_empty() {
                    out!(ctx, "{:<width$}  # {}", "", desc, width = max_key);
                }
            }
            Ok(())
        }
        "get" => {
            let key = args.get(1).map(String::as_str).unwrap_or("");
            if key.is_empty() {
                let items = crate::config::config_list();
                let max_key = items.iter().map(|(k, _, _)| k.len()).max().unwrap_or(0);
                for (key, val, is_default) in &items {
                    let display_val = if val.is_empty() {
                        "(not set)"
                    } else {
                        val.as_str()
                    };
                    let marker = if *is_default { " (default)" } else { "" };
                    out!(
                        ctx,
                        "{:<width$}  {}{}",
                        key,
                        display_val,
                        marker,
                        width = max_key
                    );
                }
                return Ok(());
            }
            ensure_valid_config_key(key)?;
            let val = crate::config::config_get(key);
            out!(
                ctx,
                "{}",
                if val.is_empty() {
                    "(not set)".to_string()
                } else {
                    val
                }
            );
            Ok(())
        }
        "set" => {
            let key = args.get(1).map(String::as_str).unwrap_or("");
            if key.is_empty() {
                bail!("Usage: sidekar config set <key> <value>");
            }
            ensure_valid_config_key(key)?;
            let raw_value = args.get(2).map(String::as_str).unwrap_or("true");
            if key == "browser" {
                if matches!(raw_value, "false" | "none" | "default") {
                    crate::config::config_delete(key)?;
                    out!(ctx, "Cleared browser preference (will use system default)");
                    return Ok(());
                }
                if find_browser_by_name(raw_value).is_none() {
                    bail!(
                        "Browser '{raw_value}' not found. Available: chrome, edge, brave, arc, vivaldi, chromium, canary"
                    );
                }
            }
            if key == "relay" && crate::config::RelayMode::parse(raw_value).is_none() {
                bail!("relay must be one of: auto, on, off");
            }
            crate::config::config_set(key, raw_value)?;
            out!(ctx, "Set {key} = {raw_value}");
            Ok(())
        }
        "reset" => {
            let key = args.get(1).map(String::as_str).unwrap_or("");
            if key.is_empty() {
                bail!("Usage: sidekar config reset <key>");
            }
            ensure_valid_config_key(key)?;
            crate::config::config_delete(key)?;
            let default = crate::config::find_key(key).unwrap().default;
            let display = if default.is_empty() {
                "(not set)"
            } else {
                default
            };
            out!(ctx, "Reset {key} to default: {display}");
            Ok(())
        }
        _ => bail!("Usage: sidekar config [list|get <key>|set <key> <value>|reset <key>]"),
    }
}

fn ensure_valid_config_key(key: &str) -> Result<()> {
    if crate::config::find_key(key).is_none() {
        let valid: Vec<&str> = crate::config::CONFIG_KEYS.iter().map(|k| k.key).collect();
        bail!(
            "Unknown config key: {key}. Valid keys: {}",
            valid.join(", ")
        );
    }
    Ok(())
}

async fn cmd_update(ctx: &mut AppContext) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    out!(ctx, "Current version: {current}");
    out!(ctx, "Checking for updates...");
    match crate::api_client::check_for_update().await {
        Ok(Some(latest)) => {
            out!(ctx, "Update available: v{latest}");
            out!(ctx, "Downloading...");
            crate::api_client::self_update(&latest).await?;
            match crate::daemon::restart_if_running() {
                Ok(true) => out!(ctx, "Daemon restarted."),
                Ok(false) => {}
                Err(e) => out!(ctx, "Updated, but failed to restart daemon: {e}"),
            }
            out!(
                ctx,
                "Updated to v{latest}. Restart sidekar to use the new version."
            );
        }
        Ok(None) => out!(ctx, "Already up to date (v{current})."),
        Err(e) => bail!("Failed to check for updates: {e}"),
    }
    Ok(())
}

fn cmd_proxy(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("log");
    let rest = args.get(1..).unwrap_or(&[]);
    match sub {
        "log" => cmd_proxy_log(ctx, rest)?,
        "show" => {
            let id = rest
                .first()
                .and_then(|s| s.parse::<i64>().ok())
                .ok_or_else(|| anyhow::anyhow!("Usage: sidekar proxy show <id>"))?;
            cmd_proxy_show(ctx, id)?;
        }
        "clear" => cmd_proxy_clear(ctx)?,
        _ => bail!("Usage: sidekar proxy <log|show|clear> [--last=N] [--json]"),
    }
    Ok(())
}

fn format_bytes(n: usize) -> String {
    if n < 1024 {
        format!("{n}B")
    } else if n < 1024 * 1024 {
        format!("{:.1}KB", n as f64 / 1024.0)
    } else {
        format!("{:.1}MB", n as f64 / (1024.0 * 1024.0))
    }
}

fn cmd_proxy_log(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let limit = args
        .iter()
        .find_map(|a| a.strip_prefix("--last="))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20);
    let json_output = args.iter().any(|a| a == "--json");

    let rows = crate::broker::proxy_log_recent(limit)?;
    if rows.is_empty() {
        out!(
            ctx,
            "No proxy log entries. Run an agent with --proxy to capture payloads."
        );
        return Ok(());
    }

    if json_output {
        let entries: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "created_at": r.created_at,
                    "method": r.method,
                    "path": r.path,
                    "upstream_host": r.upstream_host,
                    "response_status": r.response_status,
                    "duration_ms": r.duration_ms,
                    "request_size": r.request_body.len(),
                    "response_size": r.response_body.len(),
                })
            })
            .collect();
        out!(
            ctx,
            "{}",
            serde_json::to_string_pretty(&entries).unwrap_or_default()
        );
        return Ok(());
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "{:<5} {:<8} {:<6} {:<20} {:<20} {:<6} {:<8} {:<10} {}",
        "ID", "TIME", "METHOD", "PATH", "HOST", "STATUS", "DUR(ms)", "REQ", "RESP"
    ));
    for r in &rows {
        let time = {
            let secs = r.created_at % 86400;
            let h = secs / 3600;
            let m = (secs % 3600) / 60;
            let s = secs % 60;
            format!("{h:02}:{m:02}:{s:02}")
        };
        let path_short = if r.path.len() > 20 {
            format!("{}...", &r.path[..17])
        } else {
            r.path.clone()
        };
        let host_short = if r.upstream_host.len() > 20 {
            format!("{}...", &r.upstream_host[..17])
        } else {
            r.upstream_host.clone()
        };
        lines.push(format!(
            "{:<5} {:<8} {:<6} {:<20} {:<20} {:<6} {:<8} {:<10} {}",
            r.id,
            time,
            r.method,
            path_short,
            host_short,
            r.response_status,
            r.duration_ms,
            format_bytes(r.request_body.len()),
            format_bytes(r.response_body.len()),
        ));
    }
    out!(ctx, "{}", lines.join("\n"));
    Ok(())
}

fn extract_usage_from_sse(body: &[u8]) -> Option<serde_json::Value> {
    let text = std::str::from_utf8(body).ok()?;
    let mut last_usage = None;
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(data)
            && val.get("usage").is_some()
        {
            last_usage = val.get("usage").cloned();
        }
    }
    last_usage
}

fn cmd_proxy_show(ctx: &mut AppContext, id: i64) -> Result<()> {
    let row = crate::broker::proxy_log_detail(id)?
        .ok_or_else(|| anyhow::anyhow!("No proxy log entry with id {id}"))?;

    let mut lines = Vec::new();
    lines.push(format!(
        "Request #{} — {} {} → {} ({}, {}ms)",
        row.id, row.method, row.path, row.upstream_host, row.response_status, row.duration_ms
    ));

    if let Ok(req_json) = serde_json::from_slice::<serde_json::Value>(&row.request_body) {
        if let Some(model) = req_json.get("model").and_then(|v| v.as_str()) {
            lines.push(format!("Model: {model}"));
        }
        if let Some(msgs) = req_json.get("messages").and_then(|v| v.as_array()) {
            lines.push(format!("Messages: {}", msgs.len()));
        }
        if let Some(tools) = req_json.get("tools").and_then(|v| v.as_array())
            && !tools.is_empty()
        {
            lines.push(format!("Tools: {}", tools.len()));
        }
    }

    if let Some(usage) = extract_usage_from_sse(&row.response_body) {
        if let Some(input) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
            let output = usage
                .get("output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cache_read = usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cache_write = usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            lines.push(format!("Tokens: {input} in / {output} out"));
            if cache_read > 0 || cache_write > 0 {
                lines.push(format!("Cache: {cache_read} read / {cache_write} write"));
            }
        }
        if let Some(prompt) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
            let completion = usage
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            lines.push(format!("Tokens: {prompt} prompt / {completion} completion"));
        }
    }

    lines.push(format!(
        "Request body: {} | Response body: {}",
        format_bytes(row.request_body.len()),
        format_bytes(row.response_body.len()),
    ));

    lines.push(String::new());
    lines.push("--- Request Body ---".to_string());
    if let Ok(req_json) = serde_json::from_slice::<serde_json::Value>(&row.request_body) {
        let compact = crate::providers::compact_json(&req_json);
        lines.push(serde_json::to_string_pretty(&compact).unwrap_or_default());
    } else {
        let text = String::from_utf8_lossy(&row.request_body);
        lines.push(text.into_owned());
    }

    lines.push(String::new());
    lines.push("--- Response Body ---".to_string());
    let resp_text = String::from_utf8_lossy(&row.response_body);
    if resp_text.len() > 4096 {
        lines.push(format!(
            "[... {} total, showing last 4KB ...]",
            format_bytes(resp_text.len())
        ));
        let target = resp_text.len() - 4096;
        let start = (target..resp_text.len())
            .find(|&i| resp_text.is_char_boundary(i))
            .unwrap_or(resp_text.len());
        lines.push(resp_text[start..].to_string());
    } else {
        lines.push(resp_text.into_owned());
    }

    out!(ctx, "{}", lines.join("\n"));
    Ok(())
}

fn cmd_proxy_clear(ctx: &mut AppContext) -> Result<()> {
    let count = crate::broker::proxy_log_clear()?;
    out!(ctx, "Deleted {count} proxy log entries.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn event_list_accepts_positional_limit_and_json() {
        let args = args(&["list", "--json", "--level=debug", "10"]);

        let options = parse_event_list_options(&args).unwrap();

        assert_eq!(options.limit, 10);
        assert_eq!(options.level_filter, Some("debug"));
        assert!(options.json_output);
    }

    #[test]
    fn event_list_accepts_limit_flag() {
        let args = args(&["list", "--limit=1"]);

        let options = parse_event_list_options(&args).unwrap();

        assert_eq!(options.limit, 1);
    }

    #[test]
    fn event_list_rejects_unknown_option() {
        let args = args(&["list", "--bogus"]);

        let err = parse_event_list_options(&args).unwrap_err().to_string();

        assert!(err.contains("Unknown option for event list: --bogus"));
    }

    #[test]
    fn event_list_rejects_invalid_level() {
        let args = args(&["list", "--level=warn"]);

        let err = parse_event_list_options(&args).unwrap_err().to_string();

        assert!(err.contains("Invalid event level: warn"));
    }

    #[test]
    fn event_clear_rejects_unknown_option() {
        let args = args(&["clear", "10"]);

        let err = parse_event_clear_level(&args).unwrap_err().to_string();

        assert!(err.contains("Unknown option for event clear: 10"));
    }
}
