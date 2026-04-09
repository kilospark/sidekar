use super::*;

pub async fn send_cli_command(
    command: &str,
    args: &[String],
    default_tab: Option<u64>,
) -> Result<()> {
    if command == "stop" {
        return crate::daemon::stop();
    }
    if command == "status" {
        return show_status();
    }
    if command == "dev-extract" {
        return extract_extension();
    }

    let mut filtered_args = Vec::new();
    let mut target_conn: Option<u64> = None;
    let mut target_profile: Option<String> = None;
    let mut skip_next = false;
    for (i, arg) in args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--conn" {
            if let Some(val) = args.get(i + 1) {
                target_conn = Some(
                    val.parse()
                        .context("--conn requires a numeric connection ID")?,
                );
                skip_next = true;
            }
        } else if arg == "--profile" {
            if let Some(val) = args.get(i + 1) {
                target_profile = Some(val.clone());
                skip_next = true;
            }
        } else {
            filtered_args.push(arg.clone());
        }
    }

    let msg = build_command(command, &filtered_args, default_tab)?;
    crate::daemon::ensure_running()?;

    let agent_id = std::env::var("SIDEKAR_AGENT_ID").ok();
    let mut cmd_json = json!({
        "type": "ext",
        "command": msg,
    });
    if let Some(ref aid) = agent_id {
        cmd_json["agent_id"] = json!(aid);
    }
    if let Some(cid) = target_conn {
        cmd_json["conn_id"] = json!(cid);
    }
    if let Some(ref p) = target_profile {
        cmd_json["profile"] = json!(p);
    }

    if command == "watch" {
        if let Some(agent) = crate::bus::inherit_pty_registration() {
            cmd_json["deliver_to"] = json!(agent.name);
        } else {
            bail!(
                "sidekar ext watch must be run inside a sidekar-wrapped agent session \
                 (sidekar claude, sidekar codex, etc.) so events can be delivered to the bus."
            );
        }
    }

    let result = crate::daemon::send_command(&cmd_json)?;

    if let Some(err) = result.get("error").and_then(|v| v.as_str()) {
        bail!("{err}");
    }

    print_result(command, &result);
    Ok(())
}

fn show_status() -> Result<()> {
    if !crate::daemon::is_running() {
        println!("Extension bridge not running");
        return Ok(());
    }

    let status = crate::daemon::send_command(&json!({"type": "ext_status"}))?;
    let conns = status.get("connections").and_then(|v| v.as_array());

    match conns {
        Some(list) if !list.is_empty() => {
            println!("{} connection(s):", list.len());
            for c in list {
                let id = c.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                let browser = c.get("browser").and_then(|v| v.as_str()).unwrap_or("?");
                let owner = c.get("owner").and_then(|v| v.as_str());
                print!("  [{id}] {browser}");
                if let Some(o) = owner {
                    print!(" (owner: {o})");
                }
                println!();
            }
        }
        _ => {
            println!("No extension connections");
        }
    }
    Ok(())
}

fn extract_extension() -> Result<()> {
    let home = dirs::home_dir().ok_or(anyhow!("No home directory found"))?;
    let target_dir = home.join(".sidekar/extension");

    fs::create_dir_all(&target_dir).context("Failed to create .sidekar directory")?;

    let reader = Cursor::new(EXTENSION_ZIP);
    let mut archive = ZipArchive::new(reader).context("Failed to read embedded ZIP")?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).context("Failed to access ZIP entry")?;
        let outpath = target_dir.join(file.name());

        if file.name().ends_with('/') {
            fs::create_dir_all(&outpath).context("Failed to create directory in extraction")?;
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent).context("Failed to create parent directory")?;
            }
            let mut outfile = fs::File::create(&outpath).context("Failed to create output file")?;
            std::io::copy(&mut file, &mut outfile).context("Failed to copy file contents")?;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = file.unix_mode() {
                fs::set_permissions(&outpath, std::fs::Permissions::from_mode(mode))
                    .context("Failed to set file permissions")?;
            }
        }
    }

    println!(
        "Chrome extension extracted/updated to {}",
        target_dir.display()
    );
    println!(
        "To load: Chrome > Extensions > Enable Developer mode > Load unpacked > Select {}",
        target_dir.display()
    );
    Ok(())
}

fn build_command(command: &str, args: &[String], default_tab: Option<u64>) -> Result<Value> {
    fn require_tab(command: &str, explicit: Option<u64>, default_tab: Option<u64>) -> Result<u64> {
        explicit.or(default_tab).ok_or_else(|| {
            anyhow!(
                "sidekar ext {command} requires an explicit tab ID. Pass `--tab <id>` or the command's [tab_id] argument."
            )
        })
    }

    match command {
        "tabs" => Ok(json!({"command": "tabs"})),
        "read" => {
            let tab_id = require_tab(
                "read",
                args.first().and_then(|s| s.parse::<u64>().ok()),
                default_tab,
            )?;
            let mut cmd = json!({"command": "read"});
            cmd.as_object_mut()
                .unwrap()
                .insert("tabId".into(), json!(tab_id));
            Ok(cmd)
        }
        "screenshot" => {
            let tab_id = require_tab(
                "screenshot",
                args.first().and_then(|s| s.parse::<u64>().ok()),
                default_tab,
            )?;
            let mut cmd = json!({"command": "screenshot"});
            cmd.as_object_mut()
                .unwrap()
                .insert("tabId".into(), json!(tab_id));
            Ok(cmd)
        }
        "click" => {
            let target = args
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("Usage: sidekar ext click <selector|text:...>"))?;
            let tab_id = require_tab("click", None, default_tab)?;
            let mut cmd = json!({"command": "click", "target": target});
            cmd.as_object_mut()
                .unwrap()
                .insert("tabId".into(), json!(tab_id));
            Ok(cmd)
        }
        "type" => {
            if args.len() < 2 {
                bail!("Usage: sidekar ext type <selector> <text>");
            }
            let tab_id = require_tab("type", None, default_tab)?;
            let mut cmd =
                json!({"command": "type", "selector": args[0], "text": args[1..].join(" ")});
            cmd.as_object_mut()
                .unwrap()
                .insert("tabId".into(), json!(tab_id));
            Ok(cmd)
        }
        "paste" => {
            let mut html: Option<String> = None;
            let mut text: Option<String> = None;
            let mut selector: Option<String> = None;
            let mut plain_parts = Vec::new();
            let mut i = 0;
            while i < args.len() {
                match args[i].as_str() {
                    "--html" => {
                        i += 1;
                        let value = args.get(i).cloned().context(
                            "Usage: sidekar ext paste [--html <html>] [--text <text>] [--selector <selector>]",
                        )?;
                        html = Some(value);
                    }
                    "--text" => {
                        i += 1;
                        let value = args.get(i).cloned().context(
                            "Usage: sidekar ext paste [--html <html>] [--text <text>] [--selector <selector>]",
                        )?;
                        text = Some(value);
                    }
                    "--selector" => {
                        i += 1;
                        let value = args.get(i).cloned().context(
                            "Usage: sidekar ext paste [--html <html>] [--text <text>] [--selector <selector>]",
                        )?;
                        selector = Some(value);
                    }
                    other => plain_parts.push(other.to_string()),
                }
                i += 1;
            }
            if text.is_none() && !plain_parts.is_empty() {
                text = Some(plain_parts.join(" "));
            }
            if text.as_deref().unwrap_or("").is_empty() && html.as_deref().unwrap_or("").is_empty()
            {
                bail!(
                    "Usage: sidekar ext paste [--html <html>] [--text <text>] [--selector <selector>]"
                );
            }
            let tab_id = require_tab("paste", None, default_tab)?;
            let mut cmd = json!({"command": "paste", "text": text.unwrap_or_default()});
            if let Some(html) = html {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("html".into(), json!(html));
            }
            if let Some(selector) = selector {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("selector".into(), json!(selector));
            }
            cmd.as_object_mut()
                .unwrap()
                .insert("tabId".into(), json!(tab_id));
            Ok(cmd)
        }
        "set-value" => {
            if args.len() < 2 {
                bail!("Usage: sidekar ext set-value <selector> <text>");
            }
            let tab_id = require_tab("set-value", None, default_tab)?;
            let mut cmd =
                json!({"command": "setvalue", "selector": args[0], "text": args[1..].join(" ")});
            cmd.as_object_mut()
                .unwrap()
                .insert("tabId".into(), json!(tab_id));
            Ok(cmd)
        }
        "ax-tree" => {
            let tab_id = require_tab(
                "ax-tree",
                args.first().and_then(|s| s.parse::<u64>().ok()),
                default_tab,
            )?;
            let mut cmd = json!({"command": "axtree"});
            cmd.as_object_mut()
                .unwrap()
                .insert("tabId".into(), json!(tab_id));
            Ok(cmd)
        }
        "eval" => {
            let code = args.join(" ");
            if code.is_empty() {
                bail!("Usage: sidekar ext eval <javascript>");
            }
            let tab_id = require_tab("eval", None, default_tab)?;
            let mut cmd = json!({"command": "eval", "code": code});
            cmd.as_object_mut()
                .unwrap()
                .insert("tabId".into(), json!(tab_id));
            Ok(cmd)
        }
        "eval-page" => {
            let code = args.join(" ");
            if code.is_empty() {
                bail!("Usage: sidekar ext eval-page <javascript>");
            }
            let tab_id = require_tab("eval-page", None, default_tab)?;
            let mut cmd = json!({"command": "evalpage", "code": code});
            cmd.as_object_mut()
                .unwrap()
                .insert("tabId".into(), json!(tab_id));
            Ok(cmd)
        }
        "navigate" => {
            if args.is_empty() {
                bail!("Usage: sidekar ext navigate <url> [tab_id]");
            }
            let url = &args[0];
            let tab_id = require_tab(
                "navigate",
                args.get(1).and_then(|s| s.parse::<u64>().ok()),
                default_tab,
            )?;
            let mut cmd = json!({"command": "navigate", "url": url});
            cmd.as_object_mut()
                .unwrap()
                .insert("tabId".into(), json!(tab_id));
            Ok(cmd)
        }
        "new-tab" => {
            let url = args
                .first()
                .cloned()
                .unwrap_or_else(|| "about:blank".to_string());
            Ok(json!({"command": "newtab", "url": url}))
        }
        "close" => {
            let tab_id = require_tab(
                "close",
                args.first().and_then(|s| s.parse::<u64>().ok()),
                default_tab,
            )?;
            let mut cmd = json!({"command": "close"});
            cmd.as_object_mut()
                .unwrap()
                .insert("tabId".into(), json!(tab_id));
            Ok(cmd)
        }
        "scroll" => {
            let direction = args.first().map(|s| s.as_str()).unwrap_or("down");
            let tab_id = require_tab("scroll", None, default_tab)?;
            let mut cmd = json!({"command": "scroll", "direction": direction});
            cmd.as_object_mut()
                .unwrap()
                .insert("tabId".into(), json!(tab_id));
            Ok(cmd)
        }
        "history" => {
            let query = args.join(" ");
            let max_results = 25u64;
            Ok(json!({"command": "history", "query": query, "maxResults": max_results}))
        }
        "watch" => {
            let selector = args
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("Usage: sidekar ext watch <selector> [--tab <id>]"))?;
            let tab_id = require_tab("watch", None, default_tab)?;
            let mut cmd = json!({"command": "watch", "selector": selector});
            cmd.as_object_mut()
                .unwrap()
                .insert("tabId".into(), json!(tab_id));
            Ok(cmd)
        }
        "unwatch" => {
            let mut cmd = json!({"command": "unwatch"});
            if let Some(watch_id) = args.first() {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("watchId".into(), json!(watch_id));
            }
            Ok(cmd)
        }
        "watchers" => Ok(json!({"command": "watchers"})),
        "context" => Ok(json!({"command": "context"})),
        _ => bail!(
            "Unknown ext command: {command}\nAvailable: tabs, read, screenshot, click, type, paste, set-value, ax-tree, eval, eval-page, navigate, new-tab, close, scroll, history, watch, unwatch, watchers, context, status, stop"
        ),
    }
}

fn format_time_ago(ts_ms: f64) -> String {
    if ts_ms <= 0.0 {
        return "unknown".to_string();
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as f64;
    let diff_secs = ((now_ms - ts_ms) / 1000.0).max(0.0) as u64;
    if diff_secs < 60 {
        format!("{diff_secs}s ago")
    } else if diff_secs < 3600 {
        format!("{}m ago", diff_secs / 60)
    } else if diff_secs < 86400 {
        format!("{}h ago", diff_secs / 3600)
    } else {
        format!("{}d ago", diff_secs / 86400)
    }
}

fn print_result(command: &str, result: &Value) {
    match command {
        "tabs" => {
            if let Some(tabs) = result.get("tabs").and_then(|v| v.as_array()) {
                for tab in tabs {
                    let id = tab.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                    let title = tab.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    let url = tab.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let active = tab.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
                    let marker = if active { " *" } else { "" };
                    println!("[{id}]{marker} {title}");
                    println!("  {url}");
                }
                println!("\n{} tab(s)", tabs.len());
            }
        }
        "read" => {
            if let Some(title) = result.get("title").and_then(|v| v.as_str()) {
                println!("--- {} ---", title);
            }
            if let Some(url) = result.get("url").and_then(|v| v.as_str()) {
                println!("{url}\n");
            }
            if let Some(text) = result.get("text").and_then(|v| v.as_str()) {
                println!("{text}");
            }
        }
        "screenshot" => {
            if let Some(data_url) = result.get("screenshot").and_then(|v| v.as_str()) {
                if let Some(b64) = data_url.strip_prefix("data:image/jpeg;base64,") {
                    if let Ok(bytes) =
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
                    {
                        let path =
                            format!("/tmp/sidekar-ext-screenshot-{}.jpg", rand::random::<u32>());
                        if std::fs::write(&path, &bytes).is_ok() {
                            println!("Screenshot saved: {path}");
                            return;
                        }
                    }
                }
                println!("Screenshot captured ({} bytes)", data_url.len());
            }
        }
        "ax-tree" => {
            if let Some(title) = result.get("title").and_then(|v| v.as_str()) {
                println!("--- {} ---", title);
            }
            if let Some(elements) = result.get("elements").and_then(|v| v.as_array()) {
                for el in elements {
                    let r = el.get("ref").and_then(|v| v.as_u64()).unwrap_or(0);
                    let role = el.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    let name = el.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    println!("[{r}] {role}: {name}");
                }
                println!("\n{} interactive element(s)", elements.len());
            }
        }
        "navigate" => {
            if let Some(title) = result.get("title").and_then(|v| v.as_str()) {
                println!("--- {} ---", title);
            }
            if let Some(url) = result.get("url").and_then(|v| v.as_str()) {
                println!("{url}");
            }
        }
        "new-tab" => {
            let id = result.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
            let title = result.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let url = result.get("url").and_then(|v| v.as_str()).unwrap_or("");
            println!("Opened tab [{id}] {title}");
            println!("  {url}");
        }
        "close" => {
            let id = result.get("tabId").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("Closed tab [{id}]");
        }
        "paste" => {
            let mode = result
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let len = result.get("length").and_then(|v| v.as_u64()).unwrap_or(0);
            let verified = result
                .get("verified")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if verified {
                println!("Pasted {len} chars via {mode}");
            } else {
                println!("Paste attempted via {mode} ({len} chars, not verified)");
            }
            if let Some(err) = result.get("clipboard_error").and_then(|v| v.as_str()) {
                println!("Clipboard write warning: {err}");
            }
            if result
                .get("plain_text_fallback")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                println!("Used plain-text fallback for HTML content");
            }
            if let Some(from) = result.get("fallback_from").and_then(|v| v.as_str()) {
                if from != "none" {
                    println!("Fallback source: {from}");
                }
            }
            if let Some(err) = result.get("debugger_error").and_then(|v| v.as_str()) {
                println!("Debugger warning: {err}");
            }
            if let Some(err) = result.get("insert_text_error").and_then(|v| v.as_str()) {
                println!("InsertText warning: {err}");
            }
        }
        "set-value" => {
            let mode = result
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let len = result.get("length").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("Set value via {mode} ({len} chars)");
        }
        "eval-page" => {
            if let Some(value) = result.get("result") {
                if value.is_string() {
                    println!("{}", value.as_str().unwrap_or_default());
                } else {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(value).unwrap_or_default()
                    );
                }
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(result).unwrap_or_default()
                );
            }
        }
        "history" => {
            if let Some(entries) = result.get("entries").and_then(|v| v.as_array()) {
                for entry in entries {
                    let title = entry.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    let url = entry.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let visits = entry
                        .get("visitCount")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let ts = entry
                        .get("lastVisitTime")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let ago = format_time_ago(ts);
                    println!("{title}");
                    println!("  {url}");
                    println!("  {ago} | {visits} visit(s)");
                    println!();
                }
                println!("{} result(s)", entries.len());
            }
        }
        "watch" => {
            if let Some(watch_id) = result.get("watchId").and_then(|v| v.as_str()) {
                let selector = result
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                println!("Watching: {selector}");
                println!("Watch ID: {watch_id}");
                if let Some(deliver) = result.get("deliverTo").and_then(|v| v.as_str()) {
                    println!("Events will be delivered to: {deliver}");
                }
                if let Some(state) = result.get("initialState").and_then(|v| v.as_str()) {
                    if !state.is_empty() {
                        let preview = if state.len() > 200 {
                            &state[..200]
                        } else {
                            state
                        };
                        println!("Current state: {preview}");
                    }
                }
            }
        }
        "unwatch" => {
            if let Some(count) = result.get("count").and_then(|v| v.as_u64()) {
                println!("Removed {count} watcher(s)");
            } else if let Some(wid) = result.get("watchId").and_then(|v| v.as_str()) {
                println!("Removed watcher: {wid}");
            }
        }
        "watchers" => {
            if let Some(watchers) = result.get("watchers").and_then(|v| v.as_array()) {
                if watchers.is_empty() {
                    println!("No active watchers");
                } else {
                    for w in watchers {
                        let wid = w.get("watchId").and_then(|v| v.as_str()).unwrap_or("?");
                        let sel = w.get("selector").and_then(|v| v.as_str()).unwrap_or("?");
                        let tab = w.get("tabId").and_then(|v| v.as_u64()).unwrap_or(0);
                        println!("[{wid}] tab:{tab} {sel}");
                    }
                    println!("\n{} watcher(s)", watchers.len());
                }
            }
        }
        "context" => {
            if let Some(tab) = result.get("active_tab") {
                let title = tab.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let url = tab.get("url").and_then(|v| v.as_str()).unwrap_or("");
                println!("Active: {title}");
                println!("  {url}\n");
            }

            if let Some(windows) = result.get("windows").and_then(|v| v.as_object()) {
                let tab_count = result
                    .get("tab_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let win_count = result
                    .get("window_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                println!("{tab_count} tab(s) across {win_count} window(s):");
                for (wid, tabs) in windows {
                    if let Some(tabs) = tabs.as_array() {
                        println!("  Window {wid}:");
                        for t in tabs {
                            let id = t.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                            let title = t.get("title").and_then(|v| v.as_str()).unwrap_or("");
                            let active = t.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
                            let marker = if active { " *" } else { "" };
                            let short_title = if title.len() > 60 {
                                &title[..60]
                            } else {
                                title
                            };
                            println!("    [{id}]{marker} {short_title}");
                        }
                    }
                }
            }

            if let Some(history) = result.get("recent_history").and_then(|v| v.as_array()) {
                if !history.is_empty() {
                    println!("\nRecent activity:");
                    for h in history.iter().take(10) {
                        let title = h.get("title").and_then(|v| v.as_str()).unwrap_or("");
                        let url = h.get("url").and_then(|v| v.as_str()).unwrap_or("");
                        let ts = h
                            .get("lastVisitTime")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                        let ago = format_time_ago(ts);
                        let short_title = if title.len() > 50 {
                            &title[..50]
                        } else {
                            title
                        };
                        let domain = url
                            .strip_prefix("https://")
                            .or_else(|| url.strip_prefix("http://"))
                            .unwrap_or(url)
                            .split('/')
                            .next()
                            .unwrap_or("");
                        println!("  {ago} | {domain} | {short_title}");
                    }
                }
            }

            if let Some(watchers) = result.get("watchers").and_then(|v| v.as_array()) {
                if !watchers.is_empty() {
                    println!("\n{} active watcher(s)", watchers.len());
                }
            }
        }
        _ => {
            println!(
                "{}",
                serde_json::to_string_pretty(result).unwrap_or_default()
            );
        }
    }
}

#[cfg(test)]
mod tests;
