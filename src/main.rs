use sidekar::*;

#[path = "main/repl_cmd.rs"]
mod repl_cmd;
#[path = "main/top_level_cmds.rs"]
mod top_level_cmds;

fn main() {
    let raw_args: Vec<String> = env::args().skip(1).collect();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    if let Err(err) = rt.block_on(run(raw_args)) {
        eprintln!("Error: {err:#}");
        std::process::exit(1);
    }
}

async fn run(mut args: Vec<String>) -> Result<()> {
    // Split at "--": everything before is sidekar flags, everything after passes
    // through verbatim to the command/agent.
    let passthrough = if let Some(sep) = args.iter().position(|a| a == "--") {
        let after: Vec<String> = args.drain(sep..).skip(1).collect(); // skip the "--" itself
        Some(after)
    } else {
        None
    };

    // Parse global --verbose flag
    let verbose_flag = if let Some(pos) = args.iter().position(|a| a == "--verbose") {
        args.remove(pos);
        // SAFETY (`env::set_var`): The Rust standard library does not synchronize the process
        // environment. This runs before the first `.await` in `run`, so this task has not yet
        // yielded to Tokio; we only flip a diagnostic flag once at CLI startup.
        unsafe {
            std::env::set_var("SIDEKAR_VERBOSE", "1");
        }
        true
    } else {
        false
    };

    // Parse global --quiet / -q flag
    if let Some(pos) = args.iter().position(|a| a == "--quiet" || a == "-q") {
        args.remove(pos);
        sidekar::runtime::set_quiet(true);
    }

    sidekar::runtime::init(verbose_flag);

    let saw_relay = args.iter().any(|a| a == "--relay");
    let saw_no_relay = args.iter().any(|a| a == "--no-relay");
    if saw_relay && saw_no_relay {
        bail!("Use only one of: --relay, --no-relay");
    }
    let relay_override = if saw_relay {
        args.retain(|a| a != "--relay");
        Some(true)
    } else if saw_no_relay {
        args.retain(|a| a != "--no-relay");
        Some(false)
    } else {
        None
    };

    let saw_proxy = args.iter().any(|a| a == "--proxy");
    let saw_no_proxy = args.iter().any(|a| a == "--no-proxy");
    if saw_proxy && saw_no_proxy {
        bail!("Use only one of: --proxy, --no-proxy");
    }
    let proxy_override = if saw_proxy {
        args.retain(|a| a != "--proxy");
        Some(true)
    } else if saw_no_proxy {
        args.retain(|a| a != "--no-proxy");
        Some(false)
    } else {
        None
    };

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

    // Append passthrough args after sidekar flags have been consumed
    if let Some(mut pt) = passthrough {
        args.append(&mut pt);
    }

    if args.is_empty() {
        print_help();
        return Ok(());
    }

    // `sidekar --json` with no command: output version/info as JSON
    if args.len() == 1 && args[0] == "--json" {
        println!(
            "{}",
            serde_json::json!({
                "name": "sidekar",
                "version": env!("CARGO_PKG_VERSION"),
            })
        );
        return Ok(());
    }

    let raw_command = args.remove(0);
    let command = sidekar::canonical_command_name(&raw_command)
        .unwrap_or(raw_command.as_str())
        .to_string();
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
    // `sidekar <command> --help` → show help for that command
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_command_help(&command);
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

    // Initialize config on first run
    if sidekar::config::is_first_run() && !matches!(command.as_str(), "config") {
        let config = sidekar::config::SidekarConfig::default();
        let _ = sidekar::config::save_config(&config);
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

    if command == "repl" {
        return repl_cmd::handle(&args, relay_override).await;
    }
    if command == "device" {
        return top_level_cmds::handle_device(&args).await;
    }
    if command == "session" {
        return top_level_cmds::handle_session(&args).await;
    }
    if command == "browser-sessions" {
        return top_level_cmds::handle_browser_sessions(&args);
    }
    if command == "daemon" {
        return top_level_cmds::handle_daemon(&args).await;
    }
    if command == "ext" {
        return top_level_cmds::handle_ext(&args, &override_tab_id).await;
    }

    if let Some(replacement) = sidekar::removed_command_replacement(&raw_command) {
        bail!("Command '{raw_command}' was removed. Use: sidekar {replacement}");
    }

    // PTY wrapper: if the command resolves to an external binary or shell alias, launch it.
    // Only check for unknown commands — known sidekar commands must not be hijacked.
    if !sidekar::is_known_command(&command) && sidekar::pty::is_agent_command(&command) {
        return sidekar::pty::run_agent(&command, &args, relay_override, proxy_override).await;
    }
    if relay_override.is_some() || proxy_override.is_some() {
        bail!("--relay/--no-relay/--proxy/--no-proxy only apply to: sidekar <agent> [args...]");
    }
    if !sidekar::is_known_command(&command) {
        bail!("Unknown command: {command}");
    }

    let mut ctx = AppContext::new()?;

    // Fetch encryption key from server if logged in
    if !matches!(
        command.as_str(),
        "device" | "config" | "memory" | "tasks" | "compact" | "pack" | "unpack"
    ) {
        if crate::auth::auth_token().is_some() {
            if let Err(e) = crate::broker::fetch_encryption_key().await {
                eprintln!("Warning: could not fetch encryption key: {}", e);
            }
        }
    }

    // Inside a PTY wrapper — enable isolated mode (own window, no tab activation)
    if sidekar::runtime::pty_mode() {
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
    } else if sidekar::command_requires_session(&command) {
        bail!(
            "Command '{command}' requires an explicit browser session. Create one with `sidekar launch` or `sidekar connect`, list sessions with `sidekar browser-sessions list`, then rerun with `sidekar run <sessionId> {command} ...`."
        );
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

/// Parse `--tab <id>` global flag into a numeric tab id, or exit on invalid input.
fn tab_id_from_global_flag(override_tab_id: &Option<String>) -> Option<u64> {
    match override_tab_id.as_deref() {
        None => None,
        Some(s) => match s.parse::<u64>() {
            Ok(id) => Some(id),
            Err(_) => {
                eprintln!("Error: --tab requires a numeric tab ID");
                std::process::exit(1);
            }
        },
    }
}

fn format_age(timestamp: f64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let secs = (now - timestamp).max(0.0) as u64;
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}
