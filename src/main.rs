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

    let format_flag = extract_global_format_flag(&mut args)?;
    if let Some(fmt) = format_flag {
        sidekar::runtime::set_output_format(fmt);
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

    let format_is_structured = matches!(
        sidekar::runtime::output_format(),
        sidekar::output::OutputFormat::Json | sidekar::output::OutputFormat::Toon
    );

    if args.is_empty() {
        if format_is_structured {
            print_version_info();
        } else {
            print_help();
        }
        return Ok(());
    }

    let raw_command = args.remove(0);
    let command = sidekar::canonical_command_name(&raw_command)
        .unwrap_or(raw_command.as_str())
        .to_string();

    // Global --host flag: route session-requiring commands through the
    // extension daemon (which talks to your already-running Chrome) instead
    // of launching/attaching to a managed Chrome via CDP.
    let host_mode = if let Some(pos) = args.iter().position(|a| a == "--host") {
        args.remove(pos);
        true
    } else {
        false
    };

    // Global --profile <name>: select which managed Chrome profile to use.
    // Stripped from args here EXCEPT when the command consumes --profile
    // itself (launch / ext / network), so per-command parsing keeps working.
    let consumes_own_profile = matches!(command.as_str(), "launch" | "ext" | "network");
    let global_profile: Option<String> = if !consumes_own_profile {
        if let Some(pos) = args.iter().position(|a| a == "--profile") {
            if pos + 1 < args.len() {
                let val = args[pos + 1].clone();
                args.remove(pos);
                args.remove(pos);
                Some(val)
            } else {
                bail!("--profile requires a name argument");
            }
        } else {
            None
        }
    } else {
        None
    };

    if matches!(command.as_str(), "-v" | "-V" | "--version") {
        print_version_info();
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
    // `sidekar <sidekar-command> --help` → show help for that command.
    // Unknown commands may be PTY-wrapped agents, so leave their argv intact.
    if args.iter().any(|a| a == "--help" || a == "-h")
        && should_handle_sidekar_help_flag(&raw_command, &command)
    {
        print_command_help(&command);
        return Ok(());
    }
    if command == "skill" {
        sidekar::skill::print_skill();
        return Ok(());
    }
    if command == "install" {
        let mut ctx = AppContext::new()?;
        commands::dispatch(&mut ctx, "install", &args).await?;
        let buffered = ctx.drain_output();
        if !buffered.is_empty() {
            print!("{buffered}");
        }
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
        return repl_cmd::handle(&args, relay_override, proxy_override).await;
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
    ) && crate::auth::auth_token().is_some()
        && let Err(e) = crate::broker::fetch_encryption_key().await
    {
        eprintln!("Warning: could not fetch encryption key: {}", e);
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

    // Two-axis browser routing:
    //   * Whose Chrome — managed (sidekar owns the process + profile) vs host
    //     (your already-running Chrome). `--host` selects host mode; absence
    //     selects managed.
    //   * Transport — picked automatically. `--host` routes through the
    //     extension daemon (which finds the host browser); managed mode uses
    //     CDP against the launched Chrome.
    //
    // `--profile <name>` and `--host` are mutually exclusive. `--profile`
    // implies managed. Without either, managed-default is used and Chrome is
    // auto-launched on first session-requiring command.
    if host_mode && global_profile.is_some() {
        bail!(
            "--host and --profile are mutually exclusive (--host = host Chrome, --profile = managed Chrome with named profile)"
        );
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
    } else if host_mode
        && sidekar::command_requires_session(&command)
        && !is_sessionless_subcommand(&command, &args)
    {
        // --host mode: route the command through the extension daemon, which
        // talks to whichever Chrome (host or managed) the extension is
        // attached to. Tab targeting via --tab is honored if provided.
        if !sidekar::is_ext_routable_command(&command) {
            bail!(
                "--host doesn't support `{command}` (no extension equivalent yet). \
                 Drop --host to use managed Chrome, or run an --host-supported command."
            );
        }
        let default_tab = override_tab_id
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok());
        return sidekar::ext::send_cli_command(&command, &args, default_tab).await;
    } else if sidekar::command_requires_session(&command)
        && !is_sessionless_subcommand(&command, &args)
    {
        // Managed mode + no session passed: first try to reuse the last
        // session (pointed to by the per-agent last-session file). Only
        // fall back to auto-launch if no session exists or its Chrome is
        // gone — otherwise every CLI invocation would connect a new
        // session and (in isolated mode) spawn a new blank window.
        let reused = match ctx.auto_discover_last_session() {
            Ok(()) => get_debug_tabs(&ctx).await.is_ok(),
            Err(_) => false,
        };
        if !reused {
            // Clear any stale session id picked up above so launch creates
            // a fresh one cleanly.
            ctx.clear_current_session();
            let profile = global_profile.as_deref().unwrap_or("default");
            let launch_args = vec!["--profile".to_string(), profile.to_string()];
            sidekar::commands::dispatch(&mut ctx, "launch", &launch_args).await?;
            // Discard launch's structured output — the caller asked for the
            // result of the actual command, not the launch banner.
            let _ = ctx.drain_output();
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

/// Parse `--tab <id>` global flag into a numeric tab id, or exit on invalid input.
/// Subcommands on session-requiring commands that don't actually need a
/// browser session — e.g. `network passive` reads a daemon ring buffer.
fn is_sessionless_subcommand(command: &str, args: &[String]) -> bool {
    matches!(
        (command, args.first().map(String::as_str)),
        ("network", Some("passive")) | ("network", Some("sse"))
    )
}

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

fn should_handle_sidekar_help_flag(raw_command: &str, command: &str) -> bool {
    sidekar::is_known_command(command)
        || sidekar::removed_command_replacement(raw_command).is_some()
}

/// Parse the global output-format selector.
///
/// Accepts `--format=<name>` / `--format <name>`, plus shorthand `--json`
/// and `--toon`. Consumes the matching args from the vector and returns the
/// selected format (last wins). Returns `None` if no format flag was
/// provided. Unknown `--format=<name>` values return `Err`.
fn extract_global_format_flag(
    args: &mut Vec<String>,
) -> Result<Option<sidekar::output::OutputFormat>> {
    use sidekar::output::OutputFormat;
    let mut fmt: Option<OutputFormat> = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--json" {
            fmt = Some(OutputFormat::Json);
            args.remove(i);
            continue;
        }
        if a == "--toon" {
            fmt = Some(OutputFormat::Toon);
            args.remove(i);
            continue;
        }
        if a == "--markdown" || a == "--md" {
            fmt = Some(OutputFormat::Markdown);
            args.remove(i);
            continue;
        }
        if let Some(value) = a.strip_prefix("--format=") {
            let parsed = OutputFormat::parse(value)
                .ok_or_else(|| anyhow::anyhow!("Unknown format '{value}' (use text|json|toon)"))?;
            fmt = Some(parsed);
            args.remove(i);
            continue;
        }
        if a == "--format" && i + 1 < args.len() {
            let value = args[i + 1].clone();
            let parsed = OutputFormat::parse(&value)
                .ok_or_else(|| anyhow::anyhow!("Unknown format '{value}' (use text|json|toon)"))?;
            fmt = Some(parsed);
            args.remove(i);
            args.remove(i);
            continue;
        }
        i += 1;
    }
    Ok(fmt)
}

#[derive(serde::Serialize)]
struct VersionInfo {
    name: &'static str,
    version: &'static str,
}

impl sidekar::output::CommandOutput for VersionInfo {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "{}", self.version)
    }
}

fn version_info() -> VersionInfo {
    VersionInfo {
        name: "sidekar",
        version: env!("CARGO_PKG_VERSION"),
    }
}

fn print_version_info() {
    let _ = sidekar::output::emit(&version_info());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_flag_is_not_intercepted_for_unknown_agent_commands() {
        assert!(!should_handle_sidekar_help_flag("codex", "codex"));
        assert!(!should_handle_sidekar_help_flag(
            "definitely-not-sidekar",
            "definitely-not-sidekar"
        ));
    }

    #[test]
    fn help_flag_is_intercepted_for_sidekar_and_removed_commands() {
        assert!(should_handle_sidekar_help_flag("repl", "repl"));
        assert!(should_handle_sidekar_help_flag("who", "who"));
    }

    #[test]
    fn json_flag_is_extracted_before_command_selection() {
        use sidekar::output::OutputFormat;
        let mut args = vec![
            "--json".to_string(),
            "daemon".to_string(),
            "status".to_string(),
        ];

        assert_eq!(
            extract_global_format_flag(&mut args).unwrap(),
            Some(OutputFormat::Json)
        );
        assert_eq!(args, vec!["daemon", "status"]);
    }

    #[test]
    fn json_flag_is_extracted_from_command_args() {
        use sidekar::output::OutputFormat;
        let mut args = vec![
            "daemon".to_string(),
            "status".to_string(),
            "--json".to_string(),
        ];

        assert_eq!(
            extract_global_format_flag(&mut args).unwrap(),
            Some(OutputFormat::Json)
        );
        assert_eq!(args, vec!["daemon", "status"]);
    }

    #[test]
    fn format_equals_value_is_parsed() {
        use sidekar::output::OutputFormat;
        let mut args = vec!["--format=toon".to_string(), "kv".to_string()];
        assert_eq!(
            extract_global_format_flag(&mut args).unwrap(),
            Some(OutputFormat::Toon)
        );
        assert_eq!(args, vec!["kv"]);
    }

    #[test]
    fn format_space_value_is_parsed() {
        use sidekar::output::OutputFormat;
        let mut args = vec!["--format".to_string(), "json".to_string(), "kv".to_string()];
        assert_eq!(
            extract_global_format_flag(&mut args).unwrap(),
            Some(OutputFormat::Json)
        );
        assert_eq!(args, vec!["kv"]);
    }

    #[test]
    fn unknown_format_is_rejected() {
        let mut args = vec!["--format=xml".to_string(), "kv".to_string()];
        assert!(extract_global_format_flag(&mut args).is_err());
    }

    #[test]
    fn no_format_flag_returns_none() {
        let mut args = vec!["daemon".to_string(), "status".to_string()];
        assert_eq!(extract_global_format_flag(&mut args).unwrap(), None);
        assert_eq!(args, vec!["daemon", "status"]);
    }
}
