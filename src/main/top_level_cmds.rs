use sidekar::*;

/// Handle `sidekar device <login|logout|list>`.
pub async fn handle_device(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "login" => sidekar::auth::device_auth_flow().await,
        "logout" => {
            sidekar::auth::logout()?;
            println!("Signed out. Device token removed.");
            Ok(())
        }
        "list" => {
            let data = sidekar::api_client::list_devices().await?;
            if let Some(devices) = data.get("devices").and_then(|v| v.as_array()) {
                if devices.is_empty() {
                    println!("No devices registered.");
                } else {
                    println!(
                        "{:<20} {:<10} {:<8} {:<12} {}",
                        "HOSTNAME", "OS", "ARCH", "VERSION", "LAST SEEN"
                    );
                    for d in devices {
                        let hostname = d.get("hostname").and_then(|v| v.as_str()).unwrap_or("-");
                        let os = d.get("os").and_then(|v| v.as_str()).unwrap_or("-");
                        let arch = d.get("arch").and_then(|v| v.as_str()).unwrap_or("-");
                        let version = d
                            .get("sidekar_version")
                            .and_then(|v| v.as_str())
                            .unwrap_or("-");
                        let last_seen = d
                            .get("last_seen_at")
                            .and_then(|v| v.as_str())
                            .unwrap_or("-");
                        println!(
                            "{:<20} {:<10} {:<8} {:<12} {}",
                            hostname, os, arch, version, last_seen
                        );
                    }
                }
            }
            Ok(())
        }
        _ => {
            eprintln!("Usage: sidekar device <login|logout|list>");
            std::process::exit(1);
        }
    }
}

/// Handle `sidekar session list`.
pub async fn handle_session(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" => {
            let data = sidekar::api_client::list_sessions().await?;
            if let Some(sessions) = data.get("sessions").and_then(|v| v.as_array()) {
                if sessions.is_empty() {
                    println!("No active sessions.");
                } else {
                    println!(
                        "{:<20} {:<15} {:<12} {}",
                        "NAME", "AGENT", "HOSTNAME", "CWD"
                    );
                    for s in sessions {
                        let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("-");
                        let agent = s.get("agent_type").and_then(|v| v.as_str()).unwrap_or("-");
                        let hostname =
                            s.get("hostname").and_then(|v| v.as_str()).unwrap_or("-");
                        let cwd = s.get("cwd").and_then(|v| v.as_str()).unwrap_or("-");
                        println!("{:<20} {:<15} {:<12} {}", name, agent, hostname, cwd);
                    }
                }
            }
            Ok(())
        }
        _ => {
            eprintln!("Usage: sidekar session <list>");
            std::process::exit(1);
        }
    }
}

/// Handle `sidekar browser-sessions <list|show>`.
pub fn handle_browser_sessions(args: &[String]) -> Result<()> {
    let ctx = AppContext::new()?;
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" => {
            let sessions = sidekar::list_browser_sessions(&ctx)?;
            if sessions.is_empty() {
                println!("No browser sessions.");
            } else {
                println!(
                    "{:<10} {:<10} {:<12} {:<6} {:<10} {}",
                    "ID", "BROWSER", "PROFILE", "TABS", "ACTIVE", "UPDATED"
                );
                for s in sessions {
                    let browser = s.browser_name.as_deref().unwrap_or("-");
                    let profile = s.profile.as_deref().unwrap_or("default");
                    let active = s.active_tab_id.as_deref().unwrap_or("-");
                    let updated = s
                        .updated_at
                        .and_then(|ts| ts.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| super::format_age(d.as_secs_f64()))
                        .unwrap_or_else(|| "-".to_string());
                    println!(
                        "{:<10} {:<10} {:<12} {:<6} {:<10} {}",
                        s.session_id,
                        browser,
                        profile,
                        s.tabs.len(),
                        active,
                        updated
                    );
                }
            }
            Ok(())
        }
        "show" => {
            let session_id = args
                .get(1)
                .context("Usage: sidekar browser-sessions show <sessionId>")?;
            let session = sidekar::get_browser_session(&ctx, session_id)?;
            println!("id: {}", session.session_id);
            println!(
                "browser: {}",
                session.browser_name.as_deref().unwrap_or("-")
            );
            println!(
                "profile: {}",
                session.profile.as_deref().unwrap_or("default")
            );
            println!(
                "host: {}",
                session.host.as_deref().unwrap_or(sidekar::DEFAULT_CDP_HOST)
            );
            println!(
                "port: {}",
                session
                    .port
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "-".to_string())
            );
            println!(
                "active_tab: {}",
                session.active_tab_id.as_deref().unwrap_or("-")
            );
            println!(
                "tabs: {}",
                if session.tabs.is_empty() {
                    "-".to_string()
                } else {
                    session.tabs.join(", ")
                }
            );
            println!(
                "window_id: {}",
                session
                    .window_id
                    .map(|w| w.to_string())
                    .unwrap_or_else(|| "-".to_string())
            );
            println!("state_file: {}", session.state_path.display());
            println!(
                "updated: {}",
                session
                    .updated_at
                    .and_then(|ts| ts.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| super::format_age(d.as_secs_f64()))
                    .unwrap_or_else(|| "-".to_string())
            );
            println!();
            println!(
                "Run commands with: sidekar run {} <command> [args...]",
                session.session_id
            );
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
