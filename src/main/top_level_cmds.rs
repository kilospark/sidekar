use sidekar::*;

#[derive(serde::Serialize)]
struct DeviceOut {
    hostname: String,
    os: String,
    arch: String,
    version: String,
    last_seen: String,
}

#[derive(serde::Serialize)]
struct DevicesOutput {
    items: Vec<DeviceOut>,
}

impl sidekar::output::CommandOutput for DevicesOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "No devices registered.")?;
            return Ok(());
        }
        writeln!(
            w,
            "{:<20} {:<10} {:<8} {:<12} LAST SEEN",
            "HOSTNAME", "OS", "ARCH", "VERSION"
        )?;
        for d in &self.items {
            writeln!(
                w,
                "{:<20} {:<10} {:<8} {:<12} {}",
                d.hostname, d.os, d.arch, d.version, d.last_seen
            )?;
        }
        Ok(())
    }
}

/// Handle `sidekar device <login|logout|list>`.
pub async fn handle_device(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "login" => sidekar::auth::device_auth_flow().await,
        "logout" => {
            sidekar::auth::logout()?;
            sidekar::output::emit(&sidekar::output::PlainOutput::new(
                "Signed out. Device token removed.",
            ))?;
            Ok(())
        }
        "list" => {
            let data = sidekar::api_client::list_devices().await?;
            let items = data
                .get("devices")
                .and_then(|v| v.as_array())
                .map(|devices| {
                    devices
                        .iter()
                        .map(|d| DeviceOut {
                            hostname: d
                                .get("hostname")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                            os: d
                                .get("os")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                            arch: d
                                .get("arch")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                            version: d
                                .get("sidekar_version")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                            last_seen: d
                                .get("last_seen_at")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            sidekar::output::emit(&DevicesOutput { items })?;
            Ok(())
        }
        _ => {
            eprintln!("Usage: sidekar device <login|logout|list>");
            std::process::exit(1);
        }
    }
}

#[derive(serde::Serialize)]
struct RelaySessionOut {
    name: String,
    agent: String,
    hostname: String,
    cwd: String,
}

#[derive(serde::Serialize)]
struct RelaySessionsOutput {
    items: Vec<RelaySessionOut>,
}

impl sidekar::output::CommandOutput for RelaySessionsOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "No active sessions.")?;
            return Ok(());
        }
        writeln!(
            w,
            "{:<20} {:<15} {:<12} CWD",
            "NAME", "AGENT", "HOSTNAME"
        )?;
        for s in &self.items {
            writeln!(
                w,
                "{:<20} {:<15} {:<12} {}",
                s.name, s.agent, s.hostname, s.cwd
            )?;
        }
        Ok(())
    }
}

/// Handle `sidekar session list`.
pub async fn handle_session(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" => {
            let data = sidekar::api_client::list_sessions().await?;
            let items = data
                .get("sessions")
                .and_then(|v| v.as_array())
                .map(|sessions| {
                    sessions
                        .iter()
                        .map(|s| RelaySessionOut {
                            name: s
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                            agent: s
                                .get("agent_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                            hostname: s
                                .get("hostname")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                            cwd: s
                                .get("cwd")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            sidekar::output::emit(&RelaySessionsOutput { items })?;
            Ok(())
        }
        _ => {
            eprintln!("Usage: sidekar session <list>");
            std::process::exit(1);
        }
    }
}

#[derive(serde::Serialize)]
struct BrowserSessionSummary {
    id: String,
    browser: String,
    profile: String,
    tab_count: usize,
    active_tab: String,
    updated: String,
}

#[derive(serde::Serialize)]
struct BrowserSessionsOutput {
    items: Vec<BrowserSessionSummary>,
}

impl sidekar::output::CommandOutput for BrowserSessionsOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "No browser sessions.")?;
            return Ok(());
        }
        writeln!(
            w,
            "{:<10} {:<10} {:<12} {:<6} {:<10} UPDATED",
            "ID", "BROWSER", "PROFILE", "TABS", "ACTIVE"
        )?;
        for s in &self.items {
            writeln!(
                w,
                "{:<10} {:<10} {:<12} {:<6} {:<10} {}",
                s.id, s.browser, s.profile, s.tab_count, s.active_tab, s.updated
            )?;
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct BrowserSessionDetail {
    id: String,
    browser: String,
    profile: String,
    host: String,
    port: Option<u16>,
    active_tab: Option<String>,
    tabs: Vec<String>,
    window_id: Option<i64>,
    state_file: String,
    updated: String,
}

impl sidekar::output::CommandOutput for BrowserSessionDetail {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "id: {}", self.id)?;
        writeln!(w, "browser: {}", self.browser)?;
        writeln!(w, "profile: {}", self.profile)?;
        writeln!(w, "host: {}", self.host)?;
        writeln!(
            w,
            "port: {}",
            self.port
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".to_string())
        )?;
        writeln!(
            w,
            "active_tab: {}",
            self.active_tab.as_deref().unwrap_or("-")
        )?;
        writeln!(
            w,
            "tabs: {}",
            if self.tabs.is_empty() {
                "-".to_string()
            } else {
                self.tabs.join(", ")
            }
        )?;
        writeln!(
            w,
            "window_id: {}",
            self.window_id
                .map(|x| x.to_string())
                .unwrap_or_else(|| "-".to_string())
        )?;
        writeln!(w, "state_file: {}", self.state_file)?;
        writeln!(w, "updated: {}", self.updated)?;
        writeln!(w)?;
        writeln!(
            w,
            "Run commands with: sidekar run {} <command> [args...]",
            self.id
        )?;
        Ok(())
    }
}

/// Handle `sidekar browser-sessions <list|show>`.
pub fn handle_browser_sessions(args: &[String]) -> Result<()> {
    let ctx = AppContext::new()?;
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" => {
            let sessions = sidekar::list_browser_sessions(&ctx)?;
            let items = sessions
                .into_iter()
                .map(|s| BrowserSessionSummary {
                    id: s.session_id,
                    browser: s.browser_name.unwrap_or_else(|| "-".into()),
                    profile: s.profile.unwrap_or_else(|| "default".into()),
                    tab_count: s.tabs.len(),
                    active_tab: s.active_tab_id.unwrap_or_else(|| "-".into()),
                    updated: s
                        .updated_at
                        .and_then(|ts| ts.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| super::format_age(d.as_secs_f64()))
                        .unwrap_or_else(|| "-".to_string()),
                })
                .collect();
            sidekar::output::emit(&BrowserSessionsOutput { items })?;
            Ok(())
        }
        "show" => {
            let session_id = args
                .get(1)
                .context("Usage: sidekar browser-sessions show <sessionId>")?;
            let session = sidekar::get_browser_session(&ctx, session_id)?;
            let detail = BrowserSessionDetail {
                id: session.session_id,
                browser: session.browser_name.unwrap_or_else(|| "-".into()),
                profile: session.profile.unwrap_or_else(|| "default".into()),
                host: session
                    .host
                    .unwrap_or_else(|| sidekar::DEFAULT_CDP_HOST.into()),
                port: session.port,
                active_tab: session.active_tab_id,
                tabs: session.tabs,
                window_id: session.window_id,
                state_file: session.state_path.display().to_string(),
                updated: session
                    .updated_at
                    .and_then(|ts| ts.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| super::format_age(d.as_secs_f64()))
                    .unwrap_or_else(|| "-".to_string()),
            };
            sidekar::output::emit(&detail)?;
            Ok(())
        }
        _ => {
            eprintln!("Usage: sidekar browser-sessions <list|show>");
            std::process::exit(1);
        }
    }
}

/// Handle `sidekar daemon [start|stop|restart|status]`.
pub async fn handle_daemon(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "start" => sidekar::daemon::start().await,
        "relaunch" => {
            let old_pid = args
                .get(1)
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or_else(|| {
                    eprintln!("Usage: sidekar daemon relaunch <old_pid>");
                    std::process::exit(1);
                });
            sidekar::daemon::relaunch_after_exit(old_pid).await
        }
        "stop" => sidekar::daemon::stop(),
        "restart" => sidekar::daemon::restart(),
        "status" => sidekar::daemon::status(),
        "" => {
            if sidekar::daemon::is_running() {
                sidekar::daemon::status()
            } else {
                sidekar::daemon::ensure_running()
            }
        }
        _ => {
            eprintln!("Usage: sidekar daemon [start|stop|restart|status]");
            std::process::exit(1);
        }
    }
}

/// Handle `sidekar ext <subcommand>`.
pub async fn handle_ext(args: &[String], override_tab_id: &Option<String>) -> Result<()> {
    let sub = args.first().cloned().unwrap_or_default();
    if sub.is_empty() {
        eprintln!("Usage: sidekar ext <subcommand> [args...]");
        eprintln!();
        eprintln!("Browser:");
        eprintln!("  tabs                         List open tabs");
        eprintln!("  read [tab_id]                Read page text");
        eprintln!("  screenshot [tab_id]          Capture visible tab");
        eprintln!("  click <selector|text:..>     Click element");
        eprintln!("  type <selector> <text>       Type into field");
        eprintln!("  paste [--html H] [--text T]  Paste content");
        eprintln!("  set-value <selector> <text>  Set field value");
        eprintln!("  ax-tree [tab_id]             Accessibility tree");
        eprintln!("  eval <js>                    Run JS (isolated)");
        eprintln!("  eval-page <js>               Run JS (page world)");
        eprintln!("  navigate <url>               Navigate tab");
        eprintln!("  new-tab [url]                Open new tab");
        eprintln!("  close [tab_id]               Close tab");
        eprintln!("  scroll <up|down|top|bottom>  Scroll page");
        eprintln!();
        eprintln!("History & Context:");
        eprintln!("  history <query>              Search browsing history");
        eprintln!("  context                      Current browser context");
        eprintln!();
        eprintln!("Watchers (events delivered via bus):");
        eprintln!("  watch <selector>             Watch element, stream changes to bus");
        eprintln!("  unwatch [watchId]            Remove watcher(s)");
        eprintln!("  watchers                     List active watchers");
        eprintln!("  dev-extract                  Extract embedded extension ZIP");
        eprintln!();
        eprintln!("Management:");
        eprintln!("  status                       Connection status");
        eprintln!("  stop                         Stop daemon");
        eprintln!();
        eprintln!(
            "Flags: --conn <id>, --profile <name>, --tab <id> (required for tab-targeted ext commands)"
        );
        std::process::exit(1);
    }
    let sub_args = if args.len() > 1 {
        args[1..].to_vec()
    } else {
        vec![]
    };
    let default_tab = super::tab_id_from_global_flag(override_tab_id);
    sidekar::ext::send_cli_command(&sub, &sub_args, default_tab).await
}
