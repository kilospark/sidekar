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
        let Some(dest) = crate::bus::resolve_registered_agent_bus_name_for_current_process() else {
            bail!(
                "sidekar ext watch needs a broker-registered agent context: \
                 run from `sidekar claude` / `sidekar codex` / …, or `sidekar repl`, \
                 or set SIDEKAR_AGENT_NAME to your agent's bus name. \
                 (The daemon must know deliver_to to enqueue extension watch events.)"
            );
        };
        cmd_json["deliver_to"] = json!(dest);
    }

    let result = crate::daemon::send_command(&cmd_json)?;

    if let Some(err) = result.get("error").and_then(|v| v.as_str()) {
        bail!("{err}");
    }

    print_result(command, &result);
    Ok(())
}

#[derive(serde::Serialize)]
struct ExtConnectionOut {
    id: u64,
    browser: String,
    profile: String,
    owner: Option<String>,
    /// Extension manifest.json version as reported at
    /// bridge_register. None for pre-version-reporting extensions.
    #[serde(skip_serializing_if = "Option::is_none")]
    ext_version: Option<String>,
    /// True when ext_version == CARGO_PKG_VERSION. None when the
    /// extension didn't report a version (old extension binary).
    /// When false, text renderer shows a "version drift" warning so
    /// users know Chrome needs restarting after a daemon update.
    #[serde(skip_serializing_if = "Option::is_none")]
    version_matches_daemon: Option<bool>,
}

#[derive(serde::Serialize)]
struct ExtStatusOutput {
    running: bool,
    connections: Vec<ExtConnectionOut>,
}

impl crate::output::CommandOutput for ExtStatusOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if !self.running {
            writeln!(w, "Extension bridge not running")?;
            return Ok(());
        }
        if self.connections.is_empty() {
            writeln!(w, "No extension connections")?;
            return Ok(());
        }
        writeln!(w, "{} connection(s):", self.connections.len())?;
        for c in &self.connections {
            // Install IDs are 36-char UUIDs — truncate for display. Pre-UUID
            // fallback profiles (just the browser name) stay as-is.
            let short_profile = if c.profile.len() > 12 && c.profile.contains('-') {
                &c.profile[..8]
            } else {
                c.profile.as_str()
            };
            write!(w, "  [{}] {} ({})", c.id, c.browser, short_profile)?;
            if let Some(o) = &c.owner {
                write!(w, " (owner: {o})")?;
            }
            if let Some(v) = &c.ext_version {
                write!(w, " ext v{v}")?;
                // Flag drift so the user doesn't have to look at
                // logs to find out the extension and daemon aren't
                // the same version. Chrome auto-updates extensions;
                // the daemon is updated by `sidekar update`; drift
                // is normal between them until Chrome is restarted.
                if matches!(c.version_matches_daemon, Some(false)) {
                    write!(
                        w,
                        " \x1b[33m(drift — daemon v{})\x1b[0m",
                        env!("CARGO_PKG_VERSION")
                    )?;
                }
            }
            writeln!(w)?;
        }
        Ok(())
    }
}

fn show_status() -> Result<()> {
    if !crate::daemon::is_running() {
        let out = ExtStatusOutput {
            running: false,
            connections: Vec::new(),
        };
        crate::output::emit(&out)?;
        return Ok(());
    }

    let status = crate::daemon::send_command(&json!({"type": "ext_status"}))?;
    let conns = status
        .get("connections")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let connections: Vec<ExtConnectionOut> = conns
        .iter()
        .map(|c| ExtConnectionOut {
            id: c.get("id").and_then(|v| v.as_u64()).unwrap_or(0),
            browser: c
                .get("browser")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            profile: c
                .get("profile")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            owner: c
                .get("owner")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            ext_version: c
                .get("ext_version")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            version_matches_daemon: c.get("version_matches_daemon").and_then(|v| v.as_bool()),
        })
        .collect();
    let out = ExtStatusOutput {
        running: true,
        connections,
    };
    crate::output::emit(&out)?;
    Ok(())
}

fn extract_extension() -> Result<()> {
    super::extract_embedded_extension()
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

#[derive(serde::Serialize)]
struct ExtTabOut {
    id: u64,
    title: String,
    url: String,
    active: bool,
}

#[derive(serde::Serialize)]
struct ExtTabsOutput {
    items: Vec<ExtTabOut>,
}

impl crate::output::CommandOutput for ExtTabsOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        for tab in &self.items {
            let marker = if tab.active { " *" } else { "" };
            writeln!(w, "[{}]{} {}", tab.id, marker, tab.title)?;
            writeln!(w, "  {}", tab.url)?;
        }
        if !self.items.is_empty() {
            writeln!(w)?;
            writeln!(w, "{} tab(s)", self.items.len())?;
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct ExtWatcherOut {
    watch_id: String,
    selector: String,
    tab_id: u64,
}

#[derive(serde::Serialize)]
struct ExtWatchersOutput {
    items: Vec<ExtWatcherOut>,
}

impl crate::output::CommandOutput for ExtWatchersOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "No active watchers")?;
            return Ok(());
        }
        for watcher in &self.items {
            writeln!(
                w,
                "[{}] tab:{} {}",
                watcher.watch_id, watcher.tab_id, watcher.selector
            )?;
        }
        writeln!(w)?;
        writeln!(w, "{} watcher(s)", self.items.len())?;
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct ExtHistoryOut {
    title: String,
    url: String,
    visit_count: u64,
    last_visit_ms: f64,
    last_visit_ago: String,
}

#[derive(serde::Serialize)]
struct ExtHistoryOutput {
    items: Vec<ExtHistoryOut>,
}

impl crate::output::CommandOutput for ExtHistoryOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        for entry in &self.items {
            writeln!(w, "{}", entry.title)?;
            writeln!(w, "  {}", entry.url)?;
            writeln!(
                w,
                "  {} | {} visit(s)",
                entry.last_visit_ago, entry.visit_count
            )?;
            writeln!(w)?;
        }
        writeln!(w, "{} result(s)", self.items.len())?;
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct ExtContextTab {
    id: u64,
    title: String,
    url: String,
    active: bool,
}

#[derive(serde::Serialize)]
struct ExtContextWindow {
    window_id: String,
    tabs: Vec<ExtContextTab>,
}

#[derive(serde::Serialize)]
struct ExtContextOutput {
    active_tab: Option<ExtContextTab>,
    tab_count: u64,
    window_count: u64,
    windows: Vec<ExtContextWindow>,
    recent_history: Vec<ExtHistoryOut>,
    watcher_count: usize,
}

impl crate::output::CommandOutput for ExtContextOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if let Some(tab) = &self.active_tab {
            writeln!(w, "Active: {}", tab.title)?;
            writeln!(w, "  {}", tab.url)?;
            writeln!(w)?;
        }
        if !self.windows.is_empty() {
            writeln!(
                w,
                "{} tab(s) across {} window(s):",
                self.tab_count, self.window_count
            )?;
            for window in &self.windows {
                writeln!(w, "  Window {}:", window.window_id)?;
                for t in &window.tabs {
                    let marker = if t.active { " *" } else { "" };
                    let short_title = if t.title.len() > 60 {
                        &t.title[..60]
                    } else {
                        t.title.as_str()
                    };
                    writeln!(w, "    [{}]{} {}", t.id, marker, short_title)?;
                }
            }
        }
        if !self.recent_history.is_empty() {
            writeln!(w)?;
            writeln!(w, "Recent activity:")?;
            for h in &self.recent_history {
                let short_title = if h.title.len() > 50 {
                    &h.title[..50]
                } else {
                    h.title.as_str()
                };
                let domain = h
                    .url
                    .strip_prefix("https://")
                    .or_else(|| h.url.strip_prefix("http://"))
                    .unwrap_or(&h.url)
                    .split('/')
                    .next()
                    .unwrap_or("");
                writeln!(w, "  {} | {} | {}", h.last_visit_ago, domain, short_title)?;
            }
        }
        if self.watcher_count > 0 {
            writeln!(w)?;
            writeln!(w, "{} active watcher(s)", self.watcher_count)?;
        }
        Ok(())
    }
}

fn print_result(command: &str, result: &Value) {
    match command {
        "tabs" => {
            let tabs = result
                .get("tabs")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let items: Vec<ExtTabOut> = tabs
                .iter()
                .map(|tab| ExtTabOut {
                    id: tab.get("id").and_then(|v| v.as_u64()).unwrap_or(0),
                    title: tab
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    url: tab
                        .get("url")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    active: tab.get("active").and_then(|v| v.as_bool()).unwrap_or(false),
                })
                .collect();
            let _ = crate::output::emit(&ExtTabsOutput { items });
        }
        "read" => {
            let mut buf = String::new();
            if let Some(title) = result.get("title").and_then(|v| v.as_str()) {
                buf.push_str(&format!("--- {} ---\n", title));
            }
            if let Some(url) = result.get("url").and_then(|v| v.as_str()) {
                buf.push_str(&format!("{url}\n\n"));
            }
            if let Some(text) = result.get("text").and_then(|v| v.as_str()) {
                buf.push_str(&format!("{text}\n"));
            }
            // Strip trailing newline so PlainOutput's writeln doesn't double it.
            let text = buf.trim_end_matches('\n').to_string();
            let _ = crate::output::emit(&crate::output::PlainOutput::new(text));
        }
        "screenshot" => {
            if let Some(data_url) = result.get("screenshot").and_then(|v| v.as_str()) {
                if let Some(b64) = data_url.strip_prefix("data:image/jpeg;base64,")
                    && let Ok(bytes) =
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
                {
                    let path = format!("/tmp/sidekar-ext-screenshot-{}.jpg", rand::random::<u32>());
                    if std::fs::write(&path, &bytes).is_ok() {
                        let _ = crate::output::emit(&crate::output::PlainOutput::new(format!(
                            "Screenshot saved: {path}"
                        )));
                        return;
                    }
                }
                let _ = crate::output::emit(&crate::output::PlainOutput::new(format!(
                    "Screenshot captured ({} bytes)",
                    data_url.len()
                )));
            }
        }
        "ax-tree" => {
            let mut buf = String::new();
            if let Some(title) = result.get("title").and_then(|v| v.as_str()) {
                buf.push_str(&format!("--- {} ---\n", title));
            }
            if let Some(elements) = result.get("elements").and_then(|v| v.as_array()) {
                for el in elements {
                    let r = el.get("ref").and_then(|v| v.as_u64()).unwrap_or(0);
                    let role = el.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    let name = el.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    buf.push_str(&format!("[{r}] {role}: {name}\n"));
                }
                buf.push_str(&format!("\n{} interactive element(s)\n", elements.len()));
            }
            let text = buf.trim_end_matches('\n').to_string();
            let _ = crate::output::emit(&crate::output::PlainOutput::new(text));
        }
        "navigate" => {
            let mut buf = String::new();
            if let Some(title) = result.get("title").and_then(|v| v.as_str()) {
                buf.push_str(&format!("--- {} ---\n", title));
            }
            if let Some(url) = result.get("url").and_then(|v| v.as_str()) {
                buf.push_str(&format!("{url}\n"));
            }
            let text = buf.trim_end_matches('\n').to_string();
            let _ = crate::output::emit(&crate::output::PlainOutput::new(text));
        }
        "new-tab" => {
            let id = result.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
            let title = result.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let url = result.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let text = format!("Opened tab [{id}] {title}\n  {url}");
            let _ = crate::output::emit(&crate::output::PlainOutput::new(text));
        }
        "close" => {
            let id = result.get("tabId").and_then(|v| v.as_u64()).unwrap_or(0);
            let _ = crate::output::emit(&crate::output::PlainOutput::new(format!(
                "Closed tab [{id}]"
            )));
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
            let mut buf = String::new();
            if verified {
                buf.push_str(&format!("Pasted {len} chars via {mode}\n"));
            } else {
                buf.push_str(&format!(
                    "Paste attempted via {mode} ({len} chars, not verified)\n"
                ));
            }
            if let Some(err) = result.get("clipboard_error").and_then(|v| v.as_str()) {
                buf.push_str(&format!("Clipboard write warning: {err}\n"));
            }
            if result
                .get("plain_text_fallback")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                buf.push_str("Used plain-text fallback for HTML content\n");
            }
            if let Some(from) = result.get("fallback_from").and_then(|v| v.as_str())
                && from != "none"
            {
                buf.push_str(&format!("Fallback source: {from}\n"));
            }
            if let Some(err) = result.get("debugger_error").and_then(|v| v.as_str()) {
                buf.push_str(&format!("Debugger warning: {err}\n"));
            }
            if let Some(err) = result.get("insert_text_error").and_then(|v| v.as_str()) {
                buf.push_str(&format!("InsertText warning: {err}\n"));
            }
            let text = buf.trim_end_matches('\n').to_string();
            let _ = crate::output::emit(&crate::output::PlainOutput::new(text));
        }
        "set-value" => {
            let mode = result
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let len = result.get("length").and_then(|v| v.as_u64()).unwrap_or(0);
            let _ = crate::output::emit(&crate::output::PlainOutput::new(format!(
                "Set value via {mode} ({len} chars)"
            )));
        }
        "eval-page" => {
            let text = if let Some(value) = result.get("result") {
                if value.is_string() {
                    value.as_str().unwrap_or_default().to_string()
                } else {
                    serde_json::to_string_pretty(value).unwrap_or_default()
                }
            } else {
                serde_json::to_string_pretty(result).unwrap_or_default()
            };
            let _ = crate::output::emit(&crate::output::PlainOutput::new(text));
        }
        "history" => {
            let entries = result
                .get("entries")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let items: Vec<ExtHistoryOut> = entries
                .iter()
                .map(|entry| {
                    let ts = entry
                        .get("lastVisitTime")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    ExtHistoryOut {
                        title: entry
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        url: entry
                            .get("url")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        visit_count: entry
                            .get("visitCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                        last_visit_ms: ts,
                        last_visit_ago: format_time_ago(ts),
                    }
                })
                .collect();
            let _ = crate::output::emit(&ExtHistoryOutput { items });
        }
        "watch" => {
            if let Some(watch_id) = result.get("watchId").and_then(|v| v.as_str()) {
                let selector = result
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let mut buf = String::new();
                buf.push_str(&format!("Watching: {selector}\n"));
                buf.push_str(&format!("Watch ID: {watch_id}\n"));
                if let Some(deliver) = result.get("deliverTo").and_then(|v| v.as_str()) {
                    buf.push_str(&format!("Events will be delivered to: {deliver}\n"));
                }
                if let Some(state) = result.get("initialState").and_then(|v| v.as_str())
                    && !state.is_empty()
                {
                    let preview = if state.len() > 200 {
                        &state[..200]
                    } else {
                        state
                    };
                    buf.push_str(&format!("Current state: {preview}\n"));
                }
                let text = buf.trim_end_matches('\n').to_string();
                let _ = crate::output::emit(&crate::output::PlainOutput::new(text));
            }
        }
        "unwatch" => {
            let msg = if let Some(count) = result.get("count").and_then(|v| v.as_u64()) {
                Some(format!("Removed {count} watcher(s)"))
            } else {
                result
                    .get("watchId")
                    .and_then(|v| v.as_str())
                    .map(|wid| format!("Removed watcher: {wid}"))
            };
            if let Some(m) = msg {
                let _ = crate::output::emit(&crate::output::PlainOutput::new(m));
            }
        }
        "watchers" => {
            let watchers = result
                .get("watchers")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let items: Vec<ExtWatcherOut> = watchers
                .iter()
                .map(|w| ExtWatcherOut {
                    watch_id: w
                        .get("watchId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string(),
                    selector: w
                        .get("selector")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string(),
                    tab_id: w.get("tabId").and_then(|v| v.as_u64()).unwrap_or(0),
                })
                .collect();
            let _ = crate::output::emit(&ExtWatchersOutput { items });
        }
        "context" => {
            let active_tab = result.get("active_tab").map(|tab| ExtContextTab {
                id: tab.get("id").and_then(|v| v.as_u64()).unwrap_or(0),
                title: tab
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                url: tab
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                active: true,
            });

            let tab_count = result
                .get("tab_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let window_count = result
                .get("window_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let mut windows: Vec<ExtContextWindow> = Vec::new();
            if let Some(wins) = result.get("windows").and_then(|v| v.as_object()) {
                for (wid, tabs) in wins {
                    if let Some(tabs_arr) = tabs.as_array() {
                        windows.push(ExtContextWindow {
                            window_id: wid.clone(),
                            tabs: tabs_arr
                                .iter()
                                .map(|t| ExtContextTab {
                                    id: t.get("id").and_then(|v| v.as_u64()).unwrap_or(0),
                                    title: t
                                        .get("title")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    url: t
                                        .get("url")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    active: t
                                        .get("active")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false),
                                })
                                .collect(),
                        });
                    }
                }
            }

            let recent_history: Vec<ExtHistoryOut> = result
                .get("recent_history")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
                .iter()
                .take(10)
                .map(|h| {
                    let ts = h
                        .get("lastVisitTime")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    ExtHistoryOut {
                        title: h
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        url: h
                            .get("url")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        visit_count: h.get("visitCount").and_then(|v| v.as_u64()).unwrap_or(0),
                        last_visit_ms: ts,
                        last_visit_ago: format_time_ago(ts),
                    }
                })
                .collect();

            let watcher_count = result
                .get("watchers")
                .and_then(|v| v.as_array())
                .map(|w| w.len())
                .unwrap_or(0);

            let _ = crate::output::emit(&ExtContextOutput {
                active_tab,
                tab_count,
                window_count,
                windows,
                recent_history,
                watcher_count,
            });
        }
        _ => {
            let text = serde_json::to_string_pretty(result).unwrap_or_default();
            let _ = crate::output::emit(&crate::output::PlainOutput::new(text));
        }
    }
}

#[cfg(test)]
mod tests;
