use sidekar::*;

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("Error: {err:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let mut args: Vec<String> = env::args().skip(1).collect();

    // Parse global --tab <id> flag before extracting the command
    let override_tab_id = if let Some(pos) = args.iter().position(|a| a == "--tab") {
        if pos + 1 < args.len() {
            let tab_id = args[pos + 1].clone();
            args.remove(pos); // remove --tab
            args.remove(pos); // remove the id (now at same index)
            Some(tab_id)
        } else {
            eprintln!("Error: --tab requires a tab ID argument");
            std::process::exit(1);
        }
    } else {
        None
    };

    if args.is_empty() {
        print_help();
        return Ok(());
    }

    let command = args.remove(0);
    if matches!(command.as_str(), "-v" | "-V" | "--version") {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if matches!(command.as_str(), "-h" | "--help" | "help") {
        if let Some(subcmd) = args.first() {
            print_command_help(subcmd);
        } else {
            print_help();
        }
        return Ok(());
    }
    if command == "skill" {
        sidekar::skill::print_skill();
        return Ok(());
    }
    if command == "install" {
        sidekar::skill::install_skill();
        return Ok(());
    }

    // Show telemetry info on first run (when no config exists yet)
    if sidekar::config::is_first_run() && !matches!(command.as_str(), "telemetry" | "config") {
        let config = sidekar::config::SidekarConfig::default();
        let _ = sidekar::config::save_config(&config);
        eprintln!("");
        eprintln!("Thanks for installing sidekar!");
        eprintln!("");
        eprintln!("Anonymous telemetry is enabled by default to help us improve.");
        eprintln!("It collects: tool usage counts, error counts (no personal data).");
        eprintln!("");
        eprintln!("To disable: sidekar config set telemetry false");
        eprintln!("");
    }

    if command == "uninstall" {
        let mut ctx = AppContext::new()?;
        commands::dispatch(&mut ctx, "uninstall", &args).await?;
        let buffered = ctx.drain_output();
        if !buffered.is_empty() {
            print!("{buffered}");
        }
        return Ok(());
    }
    if command == "update" {
        let mut ctx = AppContext::new()?;
        commands::dispatch(&mut ctx, "update", &args).await?;
        let buffered = ctx.drain_output();
        if !buffered.is_empty() {
            print!("{buffered}");
        }
        return Ok(());
    }
    if command == "login" {
        return sidekar::auth::device_auth_flow().await;
    }
    if command == "logout" {
        sidekar::auth::logout()?;
        println!("Logged out. Device token removed.");
        return Ok(());
    }
    if command == "devices" {
        let data = sidekar::api_client::list_devices().await?;
        if let Some(devices) = data.get("devices").and_then(|v| v.as_array()) {
            if devices.is_empty() {
                println!("No devices registered.");
            } else {
                println!("{:<20} {:<10} {:<8} {:<12} {}", "HOSTNAME", "OS", "ARCH", "VERSION", "LAST SEEN");
                for d in devices {
                    let hostname = d.get("hostname").and_then(|v| v.as_str()).unwrap_or("-");
                    let os = d.get("os").and_then(|v| v.as_str()).unwrap_or("-");
                    let arch = d.get("arch").and_then(|v| v.as_str()).unwrap_or("-");
                    let version = d.get("sidekar_version").and_then(|v| v.as_str()).unwrap_or("-");
                    let last_seen = d.get("last_seen_at").and_then(|v| v.as_str()).unwrap_or("-");
                    println!("{:<20} {:<10} {:<8} {:<12} {}", hostname, os, arch, version, last_seen);
                }
            }
        }
        return Ok(());
    }
    if command == "sessions" {
        let data = sidekar::api_client::list_sessions().await?;
        if let Some(sessions) = data.get("sessions").and_then(|v| v.as_array()) {
            if sessions.is_empty() {
                println!("No active sessions.");
            } else {
                println!("{:<20} {:<15} {:<12} {}", "NAME", "AGENT", "HOSTNAME", "CWD");
                for s in sessions {
                    let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("-");
                    let agent = s.get("agent_type").and_then(|v| v.as_str()).unwrap_or("-");
                    let hostname = s.get("hostname").and_then(|v| v.as_str()).unwrap_or("-");
                    let cwd = s.get("cwd").and_then(|v| v.as_str()).unwrap_or("-");
                    println!("{:<20} {:<15} {:<12} {}", name, agent, hostname, cwd);
                }
            }
        }
        return Ok(());
    }

    // Daemon
    if command == "daemon" {
        let sub = args.first().map(|s| s.as_str()).unwrap_or("");
        match sub {
            "run" => return sidekar::daemon::run().await,
            "stop" => return sidekar::daemon::stop(),
            "status" => return sidekar::daemon::status(),
            "" => {
                // Default: show status or start
                if sidekar::daemon::is_running() {
                    return sidekar::daemon::status();
                } else {
                    return sidekar::daemon::ensure_running();
                }
            }
            _ => {
                eprintln!("Usage: sidekar daemon [run|stop|status]");
                std::process::exit(1);
            }
        }
    }

    // Extension bridge (legacy - will be absorbed into daemon)
    if command == "ext-server" {
        return sidekar::ext::run_server().await;
    }
    // Native messaging host (invoked by Chrome extension)
    if command == "native-messaging-host" {
        return sidekar::ext::run_native_host();
    }
    if command == "ext" {
        let sub = args.first().cloned().unwrap_or_default();
        if sub.is_empty() {
            eprintln!("Usage: sidekar ext <subcommand> [args...]");
            eprintln!("Subcommands: tabs, read, screenshot, click, type, axtree, eval, navigate,");
            eprintln!("  newtab, close, scroll, status, stop, install-host");
            std::process::exit(1);
        }
        // Handle install-host subcommand
        if sub == "install-host" {
            let ext_id = args.get(1).map(|s| s.as_str());
            return sidekar::ext::install_native_host(ext_id);
        }
        let sub_args = if args.len() > 1 { args[1..].to_vec() } else { vec![] };
        let default_tab = match override_tab_id.as_deref() {
            None => None,
            Some(s) => match s.parse::<u64>() {
                Ok(id) => Some(id),
                Err(_) => {
                    eprintln!("Error: --tab requires a numeric tab ID");
                    std::process::exit(1);
                }
            },
        };
        return sidekar::ext::send_cli_command(&sub, &sub_args, default_tab).await;
    }

    // PTY wrapper: if the command resolves to an external binary or shell alias, launch it.
    // Only check for unknown commands — known sidekar commands must not be hijacked.
    if !sidekar::is_known_command(&command) && sidekar::pty::is_agent_command(&command) {
        return sidekar::pty::run_agent(&command, &args).await;
    }

    // Auto-route browser commands through the Chrome extension if it is connected
    // and authenticated, unless an explicit --profile was used (which implies CDP).
    const EXT_ROUTABLE: &[&str] = &[
        "tabs", "read", "screenshot", "click", "type", "axtree",
        "eval", "navigate", "newtab", "close", "scroll",
    ];
    if EXT_ROUTABLE.contains(&command.as_str()) && sidekar::ext::is_ext_available() {
        let default_tab = match override_tab_id.as_deref() {
            None => None,
            Some(s) => match s.parse::<u64>() {
                Ok(id) => Some(id),
                Err(_) => {
                    eprintln!("Error: --tab requires a numeric tab ID");
                    std::process::exit(1);
                }
            },
        };
        return sidekar::ext::send_cli_command(&command, &args, default_tab).await;
    }

    let mut ctx = AppContext::new()?;

    // Fetch encryption key from server if logged in
    if !matches!(command.as_str(), "login") {
        if crate::auth::auth_token().is_some() {
            if let Err(e) = crate::broker::fetch_encryption_key().await {
                eprintln!("Warning: could not fetch encryption key: {}", e);
            }
        }
    }

    // Inside a PTY wrapper — enable isolated mode (own window, no tab activation)
    if env::var("SIDEKAR_PTY").is_ok() {
        ctx.isolated = true;
    }

    if let Some(port) = env::var("CDP_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
    {
        ctx.cdp_port = port;
    }

    if let Some(ref tab_id) = override_tab_id {
        ctx.override_tab_id = Some(tab_id.clone());
    }

    if command == "run" {
        let session_id = args
            .first()
            .cloned()
            .context("Usage: sidekar run <sessionId> [command args...]")?;
        ctx.set_current_session(session_id);
        ctx.hydrate_connection_from_state()?;

        if args.len() > 1 {
            let inline_command = args[1].clone();
            let inline_args = args[2..].to_vec();
            commands::dispatch(&mut ctx, &inline_command, &inline_args).await?;
        } else {
            run_command_file(&mut ctx).await?;
        }
        let buffered = ctx.drain_output();
        if !buffered.is_empty() {
            print!("{buffered}");
        }
        return Ok(());
    }

    if ctx.override_tab_id.is_some() {
        // --tab mode: discover Chrome port only, then create an isolated session
        // to avoid polluting the original session's state (ref maps, frame, etc.)
        let port = if let Ok(state_port) = (|| -> Result<u16> {
            let sid = fs::read_to_string(ctx.last_session_file())?
                .trim()
                .to_string();
            let path = ctx.session_state_file(&sid);
            let content = fs::read_to_string(&path)?;
            let state: serde_json::Value = serde_json::from_str(&content)?;
            state
                .get("port")
                .and_then(|v| v.as_u64())
                .map(|p| p as u16)
                .ok_or_else(|| anyhow!("no port"))
        })() {
            state_port
        } else {
            // No session — try reading port from default profile
            let port_file = ctx.chrome_port_file_for("default");
            let port_str = fs::read_to_string(&port_file)
                .context("No running browser found. Run: sidekar launch")?;
            port_str
                .trim()
                .parse::<u16>()
                .context("No running browser found. Run: sidekar launch")?
        };
        ctx.cdp_port = port;
        // Validate Chrome is actually reachable on this port
        if get_debug_tabs(&ctx).await.is_err() {
            bail!("No running browser found. Run: sidekar launch");
        }
        // Isolated session ID — never reuses an existing session's state file
        let tab_id = ctx.override_tab_id.as_ref().unwrap();
        let short = &tab_id[..tab_id.len().min(8)];
        ctx.set_current_session(format!("tab-{short}"));
    } else if !matches!(command.as_str(),
        "launch" | "connect" | "config" | "update"
        | "who" | "bus_send" | "bus-send" | "bus_done" | "bus-done"
        | "feedback" | "errors" | "telemetry"
        | "cron_create" | "cron-create" | "cron_list" | "cron-list" | "cron_delete" | "cron-delete"
        | "desktop-screenshot" | "desktop_screenshot"
        | "desktop-apps" | "desktop_apps"
        | "desktop-windows" | "desktop_windows"
        | "desktop-find" | "desktop_find"
        | "desktop-click" | "desktop_click"
        | "desktop-launch" | "desktop_launch"
        | "desktop-activate" | "desktop_activate"
        | "desktop-quit" | "desktop_quit"
        | "totp"
        | "kv"
    ) {
        if ctx.auto_discover_last_session().is_err() {
            // No session — auto-launch Chrome if inside a PTY wrapper or
            // if the command clearly needs a browser.
            let in_pty = env::var("SIDEKAR_PTY").is_ok();
            let needs_browser = !matches!(
                command.as_str(),
                "feedback"
                    | "telemetry"
                    | "who"
                    | "bus_send"
                    | "bus_done"
                    | "desktop-screenshot"
                    | "desktop_screenshot"
                    | "desktop-apps"
                    | "desktop_apps"
                    | "desktop-windows"
                    | "desktop_windows"
                    | "desktop-find"
                    | "desktop_find"
                    | "desktop-click"
                    | "desktop_click"
                    | "desktop-launch"
                    | "desktop_launch"
                    | "desktop-activate"
                    | "desktop_activate"
                    | "desktop-quit"
                    | "desktop_quit"
                    | "totp"
                    | "kv"
            );
            if in_pty && needs_browser {
                eprintln!("sidekar: no active session, auto-launching Chrome...");
                commands::dispatch(&mut ctx, "launch", &[]).await?;
                ctx.output.clear();
            } else {
                bail!("No active session. Run: sidekar launch");
            }
        }
    }

    commands::dispatch(&mut ctx, &command, &args).await?;
    let buffered = ctx.drain_output();
    if !buffered.is_empty() {
        print!("{buffered}");
    }
    Ok(())
}

async fn run_command_file(ctx: &mut AppContext) -> Result<()> {
    let session_id = ctx.require_session_id()?.to_string();
    let cmd_file = ctx.command_file(&session_id);
    let content = fs::read_to_string(&cmd_file)
        .with_context(|| format!("Cannot read {}", cmd_file.display()))?;
    let parsed: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Invalid JSON in {}", cmd_file.display()))?;

    let entries = if parsed.is_array() {
        serde_json::from_value::<Vec<CommandFileEntry>>(parsed)?
    } else {
        vec![serde_json::from_value::<CommandFileEntry>(parsed)?]
    };

    for entry in entries {
        if entry.command.trim().is_empty() {
            bail!("Missing \"command\" field in command file");
        }
        let args = entry.args.iter().map(json_value_to_arg).collect::<Vec<_>>();
        commands::dispatch(ctx, &entry.command, &args).await?;
    }

    Ok(())
}
