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

#[derive(serde::Serialize)]
struct EventRowOut {
    id: i64,
    created_at: i64,
    level: String,
    source: String,
    message: String,
    details: Option<String>,
}

#[derive(serde::Serialize)]
struct EventListOutput {
    items: Vec<EventRowOut>,
}

impl crate::output::CommandOutput for EventListOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "No events.")?;
            return Ok(());
        }
        writeln!(w, "id\tcreated_at\tlevel\tsource\tmessage")?;
        for r in &self.items {
            let details = r.details.as_deref().unwrap_or("");
            let msg = if details.is_empty() {
                r.message.clone()
            } else {
                format!("{} | {}", r.message, details)
            };
            writeln!(
                w,
                "{}\t{}\t{}\t{}\t{}",
                r.id, r.created_at, r.level, r.source, msg
            )?;
        }
        Ok(())
    }
}

fn cmd_event(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" => {
            let options = parse_event_list_options(args)?;
            let rows = crate::broker::events_recent(options.limit, options.level_filter)?;
            let output = EventListOutput {
                items: rows
                    .into_iter()
                    .map(|r| EventRowOut {
                        id: r.id,
                        created_at: r.created_at,
                        level: r.level,
                        source: r.source,
                        message: r.message,
                        details: r.details,
                    })
                    .collect(),
            };
            out!(ctx, "{}", crate::output::to_string(&output)?);
            Ok(())
        }
        "clear" => {
            let level_filter = parse_event_clear_level(args)?;
            let deleted = crate::broker::events_clear(level_filter)?;
            let msg = format!("Deleted {deleted} events.");
            out!(
                ctx,
                "{}",
                crate::output::to_string(&crate::output::PlainOutput::new(msg))?
            );
            Ok(())
        }
        _ => bail!("Unknown subcommand: event {sub}. Use: event list, event clear"),
    }
}

#[derive(Debug)]
struct EventListOptions<'a> {
    limit: usize,
    level_filter: Option<&'a str>,
}

fn parse_event_list_options(args: &[String]) -> Result<EventListOptions<'_>> {
    let mut options = EventListOptions {
        limit: 50,
        level_filter: None,
    };
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
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

#[derive(serde::Serialize)]
struct ConfigEntryOut {
    key: String,
    value: String,
    is_default: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

#[derive(serde::Serialize)]
struct ConfigListOutput {
    items: Vec<ConfigEntryOut>,
}

impl crate::output::CommandOutput for ConfigListOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        let max_key = self.items.iter().map(|e| e.key.len()).max().unwrap_or(0);
        for entry in &self.items {
            let display_val = if entry.value.is_empty() {
                "(not set)"
            } else {
                entry.value.as_str()
            };
            let marker = if entry.is_default { " (default)" } else { "" };
            writeln!(
                w,
                "{:<width$}  {}{}",
                entry.key,
                display_val,
                marker,
                width = max_key
            )?;
            if let Some(desc) = entry.description.as_deref()
                && !desc.is_empty()
            {
                writeln!(w, "{:<width$}  # {}", "", desc, width = max_key)?;
            }
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct ConfigGetOutput {
    key: String,
    value: String,
    is_set: bool,
}

impl crate::output::CommandOutput for ConfigGetOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.is_set {
            writeln!(w, "{}", self.value)
        } else {
            writeln!(w, "(not set)")
        }
    }
}

fn build_config_list_output(include_descriptions: bool) -> ConfigListOutput {
    let items = crate::config::config_list();
    ConfigListOutput {
        items: items
            .into_iter()
            .map(|(key, value, is_default)| {
                let description = if include_descriptions {
                    crate::config::find_key(&key)
                        .map(|k| k.description.to_string())
                        .filter(|s| !s.is_empty())
                } else {
                    None
                };
                ConfigEntryOut {
                    key,
                    value,
                    is_default,
                    description,
                }
            })
            .collect(),
    }
}

fn cmd_config(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("list");
    match action {
        "list" | "ls" => {
            let output = build_config_list_output(true);
            out!(ctx, "{}", crate::output::to_string(&output)?);
            Ok(())
        }
        "get" => {
            let key = args.get(1).map(String::as_str).unwrap_or("");
            if key.is_empty() {
                let output = build_config_list_output(false);
                out!(ctx, "{}", crate::output::to_string(&output)?);
                return Ok(());
            }
            ensure_valid_config_key(key)?;
            let val = crate::config::config_get(key);
            let output = ConfigGetOutput {
                key: key.to_string(),
                is_set: !val.is_empty(),
                value: val,
            };
            out!(ctx, "{}", crate::output::to_string(&output)?);
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
                    let msg = "Cleared browser preference (will use system default)".to_string();
                    out!(
                        ctx,
                        "{}",
                        crate::output::to_string(&crate::output::PlainOutput::new(msg))?
                    );
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
            let msg = format!("Set {key} = {raw_value}");
            out!(
                ctx,
                "{}",
                crate::output::to_string(&crate::output::PlainOutput::new(msg))?
            );
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
            let msg = format!("Reset {key} to default: {display}");
            out!(
                ctx,
                "{}",
                crate::output::to_string(&crate::output::PlainOutput::new(msg))?
            );
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

#[derive(serde::Serialize)]
struct UpdateOutput {
    current: String,
    latest: Option<String>,
    updated: bool,
    daemon_restarted: bool,
    daemon_restart_error: Option<String>,
}

impl crate::output::CommandOutput for UpdateOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "Current version: {}", self.current)?;
        writeln!(w, "Checking for updates...")?;
        match &self.latest {
            Some(latest) if self.updated => {
                writeln!(w, "Update available: v{latest}")?;
                writeln!(w, "Downloading...")?;
                if self.daemon_restarted {
                    writeln!(w, "Daemon restarted.")?;
                }
                if let Some(err) = &self.daemon_restart_error {
                    writeln!(w, "Updated, but failed to restart daemon: {err}")?;
                }
                writeln!(
                    w,
                    "Updated to v{latest}. Restart sidekar to use the new version."
                )?;
            }
            _ => {
                writeln!(w, "Already up to date (v{}).", self.current)?;
            }
        }
        Ok(())
    }
}

async fn cmd_update(ctx: &mut AppContext) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let output = match crate::api_client::check_for_update().await {
        Ok(Some(latest)) => {
            crate::api_client::self_update(&latest).await?;
            let (daemon_restarted, daemon_restart_error) = match crate::daemon::restart_if_running()
            {
                Ok(true) => (true, None),
                Ok(false) => (false, None),
                Err(e) => (false, Some(e.to_string())),
            };
            UpdateOutput {
                current,
                latest: Some(latest),
                updated: true,
                daemon_restarted,
                daemon_restart_error,
            }
        }
        Ok(None) => UpdateOutput {
            current,
            latest: None,
            updated: false,
            daemon_restarted: false,
            daemon_restart_error: None,
        },
        Err(e) => bail!("Failed to check for updates: {e}"),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
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
        _ => bail!("Usage: sidekar proxy <log|show|clear> [--last=N]"),
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

#[derive(serde::Serialize)]
struct ProxyLogEntryOut {
    id: i64,
    created_at: i64,
    method: String,
    path: String,
    upstream_host: String,
    response_status: i64,
    duration_ms: i64,
    request_size: usize,
    response_size: usize,
}

#[derive(serde::Serialize)]
struct ProxyLogOutput {
    items: Vec<ProxyLogEntryOut>,
}

impl crate::output::CommandOutput for ProxyLogOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(
                w,
                "No proxy log entries. Run an agent with --proxy to capture payloads."
            )?;
            return Ok(());
        }
        writeln!(
            w,
            "{:<5} {:<8} {:<6} {:<20} {:<20} {:<6} {:<8} {:<10} RESP",
            "ID", "TIME", "METHOD", "PATH", "HOST", "STATUS", "DUR(ms)", "REQ"
        )?;
        for r in &self.items {
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
            writeln!(
                w,
                "{:<5} {:<8} {:<6} {:<20} {:<20} {:<6} {:<8} {:<10} {}",
                r.id,
                time,
                r.method,
                path_short,
                host_short,
                r.response_status,
                r.duration_ms,
                format_bytes(r.request_size),
                format_bytes(r.response_size),
            )?;
        }
        Ok(())
    }
}

fn cmd_proxy_log(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let limit = args
        .iter()
        .find_map(|a| a.strip_prefix("--last="))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20);

    let rows = crate::broker::proxy_log_recent(limit)?;
    let output = ProxyLogOutput {
        items: rows
            .into_iter()
            .map(|r| ProxyLogEntryOut {
                id: r.id,
                created_at: r.created_at,
                method: r.method,
                path: r.path,
                upstream_host: r.upstream_host,
                response_status: r.response_status,
                duration_ms: r.duration_ms,
                request_size: r.request_body.len(),
                response_size: r.response_body.len(),
            })
            .collect(),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
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

    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(lines.join("\n")))?
    );
    Ok(())
}

fn cmd_proxy_clear(ctx: &mut AppContext) -> Result<()> {
    let count = crate::broker::proxy_log_clear()?;
    let msg = format!("Deleted {count} proxy log entries.");
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(msg))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn event_list_accepts_positional_limit_and_level() {
        let args = args(&["list", "--level=debug", "10"]);

        let options = parse_event_list_options(&args).unwrap();

        assert_eq!(options.limit, 10);
        assert_eq!(options.level_filter, Some("debug"));
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
