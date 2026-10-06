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
        let code = err
            .downcast_ref::<sidekar::utils::ExitWith>()
            .map_or(1, |e| e.code);
        std::process::exit(code);
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

    // Sidekar-owned, stripped from anywhere in argv like --relay/--proxy so it can
    // sit before or after the agent name.
    let yolo = args.iter().any(|a| a == "--yolo" || a == "--auto-approve");
    if yolo {
        args.retain(|a| a != "--yolo" && a != "--auto-approve");
    }

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

    // Sidekar's browser flags, taken before the passthrough args after `--`
    // are added back, so a `--profile` there (for an agent or a script) is
    // never mistaken for sidekar's. See `take_browser_flags`.
    let browser_flags = take_browser_flags(&mut args, |name| {
        sidekar::is_known_command(sidekar::canonical_command_name(name).unwrap_or(name))
    })?;

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

    // --host routes session-requiring commands through the extension daemon
    // (which talks to your already-running Chrome) instead of launching or
    // attaching to a managed Chrome over CDP.
    let host_mode = browser_flags.host;

    // --profile picks the managed Chrome profile. `browser launch`, `browser
    // network`, `browser ext` and `network` read it from their own args, so it
    // goes back to them as `--profile <name>`, the form they parse. For every
    // other command it selects the profile here.
    let consumes_own_profile = match command.as_str() {
        "network" => true,
        "browser" => matches!(
            args.first().map(String::as_str),
            Some("launch") | Some("network") | Some("ext")
        ),
        _ => false,
    };
    let global_profile = match browser_flags.profile {
        Some(profile) if consumes_own_profile => {
            args.push("--profile".to_string());
            args.push(profile);
            None
        }
        profile => profile,
    };

    if matches!(command.as_str(), "-v" | "-V" | "--version") {
        print_version_info();
        return Ok(());
    }
    if matches!(command.as_str(), "-h" | "--help" | "help") {
        if let Some(subcmd) = args.first() {
            if subcmd == "browser" && args.len() > 1 {
                print_command_help(&args[1..].join(" "));
            } else {
                print_command_help(subcmd);
            }
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
        if command == "browser" {
            if let Some(sub) = args
                .iter()
                .find(|a| *a != "--help" && *a != "-h")
                .map(String::as_str)
            {
                print_command_help(sub);
            } else {
                print_command_help("browser");
            }
        } else {
            print_command_help(&command);
        }
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
    if command == "relay" {
        return top_level_cmds::handle_relay(&args).await;
    }
    if command == "daemon" {
        return top_level_cmds::handle_daemon(&args).await;
    }
    if command == "session" {
        return sidekar::hosted::cli::handle(&args).await;
    }
    if command == "mcp" {
        return sidekar::mcp::handle(&args).await;
    }
    // Hidden: the detached sync push worker (see
    // `commands::spawn_detached_sync_push`). Routed here, not through the
    // command table, which does not know it — as `dispatch` it was refused
    // as an unknown command, silently, since the worker's output is null.
    if command == "_sync_push" {
        return sidekar::broker::run_sync_push_worker().await;
    }

    if let Some(replacement) = sidekar::removed_command_replacement(&raw_command) {
        bail!("Command '{raw_command}' was removed. Use: sidekar {replacement}");
    }

    // PTY wrapper: if the command resolves to an external binary or shell alias, launch it.
    // Only check for unknown commands — known sidekar commands must not be hijacked.
    if !sidekar::is_known_command(&command) && sidekar::pty::is_agent_command(&command) {
        if global_profile.is_some() || host_mode {
            bail!(
                "--profile and --host pick the browser for `sidekar browser` commands; they do \
                 nothing for `sidekar {command}`. To pass one to {command}, put it after the \
                 agent name."
            );
        }
        return sidekar::pty::run_agent(&command, &args, relay_override, proxy_override, yolo)
            .await;
    }
    if command == "spawn" {
        // Spawn runs `sidekar <agent>`, so the wrapper's flags are its flags
        // too. They were taken off argv above; put them back for it to pass on.
        let flags = [
            relay_override.map(|on| if on { "--relay" } else { "--no-relay" }),
            proxy_override.map(|on| if on { "--proxy" } else { "--no-proxy" }),
            yolo.then_some("--yolo"),
        ];
        for flag in flags.into_iter().flatten().rev() {
            args.insert(0, flag.to_string());
        }
    } else if relay_override.is_some() || proxy_override.is_some() || yolo {
        bail!(
            "--relay/--no-relay/--proxy/--no-proxy/--yolo only apply to: sidekar <agent> [args...] \
             and sidekar spawn"
        );
    }
    if !sidekar::is_known_command(&command) {
        bail!("Unknown command: {command}");
    }

    let mut ctx = AppContext::new()?;
    if let Some(profile) = &global_profile {
        // The managed Chrome this command drives. It scopes the last-session
        // pointer (`AppContext::last_session_file`), so the command reuses this
        // profile's session and never another profile's.
        ctx.current_profile = profile.clone();
        ctx.profile_explicit = true;
    }

    // Fetch encryption key from server if logged in
    if !matches!(
        command.as_str(),
        "device"
            | "config"
            | "prompt"
            // Not `memory`: it syncs across devices (#31), so its commands
            // pull, the same as kv and totp.
            | "tasks"
            | "compact"
            | "pack"
            | "unpack"
            // Reads only the local registry, which is not user-scoped. In
            // --watch it would otherwise make a network round trip per refresh.
            | "agents"
    ) && crate::auth::auth_token().is_some()
    {
        match crate::broker::fetch_encryption_key().await {
            Err(e) => eprintln!("Warning: could not fetch encryption key: {}", e),
            Ok(_) => {
                if let Some(uid) = crate::broker::current_user_id()
                    && let Err(e) = crate::broker::sync_bootstrap(&uid).await
                {
                    eprintln!("Warning: could not sync secrets: {}", e);
                }
            }
        }
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

    // Two-axis browser routing:
    //   * Whose Chrome — managed (sidekar owns the process + profile) vs host
    //   * `--host` — sugar for extension transport on CDP-overlapping subs (not `browser ext`).
    //     `--tab` with `--host` uses Chrome extension tab IDs (`browser ext tabs`).
    //   * Managed CDP — default. `--tab` without `--host` uses CDP target IDs (`browser tabs`).
    //
    // `--profile <name>` and `--host` are mutually exclusive. `--profile`
    // implies managed. Without either, managed-default is used and Chrome is
    // auto-launched on first session-requiring command.
    if host_mode && global_profile.is_some() {
        bail!(
            "--host and --profile are mutually exclusive (--host = host Chrome, --profile = managed Chrome with named profile)"
        );
    }

    if command == "browser" && host_mode {
        if let Some(msg) = sidekar::command_catalog::browser_host_incompatible(&args) {
            bail!("{msg}");
        }
        if sidekar::command_catalog::browser_host_routable(&args)
            && !is_sessionless_subcommand(&command, &args)
        {
            let sub = args
                .first()
                .context("Usage: sidekar browser <subcommand> [args...]")?;
            let handler = sidekar::command_catalog::browser_subcommand_handler(sub)
                .context("Unknown browser subcommand")?;
            let default_tab = sidekar::commands::browser_ext::extension_tab_id_from_ctx(&ctx);
            return sidekar::ext::send_cli_command(handler, &args[1..], default_tab).await;
        }
        if let Some(sub) = args.first().map(String::as_str) {
            if sidekar::browser_requires_session(&args)
                && !is_sessionless_subcommand(&command, &args)
                && !matches!(sub, "ext" | "run" | "sessions")
            {
                bail!(
                    "--host doesn't support `browser {sub}` (no extension equivalent yet). \
                     Drop --host to use managed Chrome, use `sidekar browser ext {sub}` if available, \
                     or pick an --host-supported CDP subcommand."
                );
            }
        }
    }

    if ctx.override_tab_id.is_some() && !host_mode {
        // --tab mode: discover Chrome port only, then create an isolated session
        // to avoid polluting the original session's state (ref maps, frame, etc.)
        let port = if let Ok(state_port) = (|| -> Result<u16> {
            let sid = fs::read_to_string(ctx.last_session_file())?
                .trim()
                .to_string();
            let path = ctx.session_state_file(&sid);
            let content = fs::read_to_string(&path)?;
            let state: serde_json::Value = serde_json::from_str(&content)?;
            if ctx.profile_explicit {
                // Only this profile's own session (see `auto_discover_last_session`).
                let profile = state
                    .get("profile")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                if sidekar::app_context::base_profile(profile)
                    != sidekar::app_context::base_profile(&ctx.current_profile)
                {
                    bail!("the last session belongs to another profile");
                }
            }
            state
                .get("port")
                .and_then(|v| v.as_u64())
                .map(|p| p as u16)
                .ok_or_else(|| anyhow!("no port"))
        })() {
            state_port
        } else {
            // No session: try the selected profile's port (default unless --profile).
            let port_file = ctx.chrome_port_file_for(&ctx.current_profile);
            let port_str = fs::read_to_string(&port_file)
                .context("No running browser found. Run: sidekar browser launch")?;
            port_str
                .trim()
                .parse::<u16>()
                .context("No running browser found. Run: sidekar browser launch")?
        };
        ctx.cdp_port = port;
        // Validate Chrome is actually reachable on this port
        if get_debug_tabs(&ctx).await.is_err() {
            bail!("No running browser found. Run: sidekar browser launch");
        }
        // Isolated session ID — never reuses an existing session's state file
        let tab_id = ctx.override_tab_id.as_ref().unwrap();
        let short = &tab_id[..tab_id.len().min(8)];
        ctx.set_current_session(format!("tab-{short}"));
    } else if command == "browser"
        && sidekar::browser_requires_session(&args)
        && !is_sessionless_subcommand(&command, &args)
        && !matches!(args.first().map(String::as_str), Some("ext" | "run"))
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
            let launch_args = vec![
                "launch".to_string(),
                "--profile".to_string(),
                profile.to_string(),
            ];
            sidekar::commands::dispatch(&mut ctx, "browser", &launch_args).await?;
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

/// Subcommands on session-requiring commands that don't actually need a
/// browser session — e.g. `network passive` reads a daemon ring buffer.
fn is_sessionless_subcommand(command: &str, args: &[String]) -> bool {
    command == "browser"
        && matches!(
            (
                args.first().map(String::as_str),
                args.get(1).map(String::as_str)
            ),
            (Some("network"), Some("passive")) | (Some("network"), Some("sse"))
        )
}

/// Sidekar's own browser flags, taken out of argv before the command runs.
#[derive(Debug, Default, PartialEq, Eq)]
struct BrowserFlags {
    /// `--profile <name>` or `--profile=<name>`: the managed Chrome profile.
    profile: Option<String>,
    /// `--host`: drive your own Chrome through the extension instead.
    host: bool,
}

/// Take `--profile <name>`, `--profile=<name>` and `--host` out of `args`
/// (argv before any `--`).
///
/// Before the command they are always sidekar's, which is where the help puts
/// them. They used to be read only after the command was taken, so a leading
/// `--profile` became the command ("Unknown command: --profile"), and
/// `--profile=<name>` was never recognised at all: it quietly fell back to the
/// default profile, cookies and all. After the command they are sidekar's only
/// for a sidekar command; an agent run through the PTY wrapper (`sidekar codex
/// --profile work`) owns the rest of its argv.
fn take_browser_flags(
    args: &mut Vec<String>,
    is_sidekar_command: impl Fn(&str) -> bool,
) -> Result<BrowserFlags> {
    // Where the command is: past any leading browser flags, and the name a
    // leading `--profile` takes.
    let mut command_at = 0;
    while let Some(arg) = args.get(command_at) {
        command_at += match arg.as_str() {
            "--profile" => 2,
            "--host" => 1,
            a if a.starts_with("--profile=") => 1,
            _ => break,
        };
    }
    let mut end = match args.get(command_at) {
        Some(command) if !is_sidekar_command(command) => command_at,
        _ => args.len(),
    };

    let mut flags = BrowserFlags::default();
    let mut i = 0;
    while i < end {
        if args[i] == "--host" {
            args.remove(i);
            end -= 1;
            flags.host = true;
        } else if args[i] == "--profile" {
            if i + 1 >= end {
                bail!("--profile requires a name, e.g. --profile work");
            }
            flags.profile = Some(args.remove(i + 1));
            args.remove(i);
            end -= 2;
        } else if let Some(name) = args[i].strip_prefix("--profile=") {
            if name.is_empty() {
                bail!("--profile requires a name, e.g. --profile=work");
            }
            flags.profile = Some(name.to_string());
            args.remove(i);
            end -= 1;
        } else {
            i += 1;
        }
    }
    Ok(flags)
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

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn sidekar_command(name: &str) -> bool {
        matches!(name, "browser" | "network" | "kv")
    }

    fn take(items: &[&str]) -> (BrowserFlags, Vec<String>) {
        let mut args = strings(items);
        let flags = take_browser_flags(&mut args, sidekar_command).unwrap();
        (flags, args)
    }

    #[test]
    fn a_profile_before_the_command_is_sidekars_in_either_form() {
        for given in [&["--profile", "work", "browser", "tabs"][..], &["--profile=work", "browser", "tabs"]] {
            let (flags, rest) = take(given);
            assert_eq!(flags.profile.as_deref(), Some("work"), "{given:?}");
            assert_eq!(rest, strings(&["browser", "tabs"]), "{given:?}");
        }
    }

    #[test]
    fn a_profile_after_a_sidekar_command_is_sidekars_in_either_form() {
        for given in [&["browser", "--profile", "work", "tabs"][..], &["browser", "tabs", "--profile=work"]] {
            let (flags, rest) = take(given);
            assert_eq!(flags.profile.as_deref(), Some("work"), "{given:?}");
            assert_eq!(rest, strings(&["browser", "tabs"]), "{given:?}");
        }
    }

    #[test]
    fn host_is_taken_before_or_after_a_sidekar_command() {
        for given in [&["--host", "browser", "tabs"][..], &["browser", "tabs", "--host"]] {
            let (flags, rest) = take(given);
            assert!(flags.host, "{given:?}");
            assert_eq!(rest, strings(&["browser", "tabs"]), "{given:?}");
        }
    }

    #[test]
    fn an_agent_keeps_its_own_flags() {
        // `sidekar codex --profile work` is Codex's own --profile.
        let (flags, rest) = take(&["codex", "--profile", "work", "--host"]);
        assert_eq!(flags, BrowserFlags::default());
        assert_eq!(rest, strings(&["codex", "--profile", "work", "--host"]));
    }

    #[test]
    fn a_flag_placed_before_an_agent_is_taken_so_it_can_be_refused() {
        let (flags, rest) = take(&["--profile", "work", "codex", "--profile", "theirs"]);
        assert_eq!(flags.profile.as_deref(), Some("work"));
        assert_eq!(rest, strings(&["codex", "--profile", "theirs"]));
    }

    #[test]
    fn the_last_profile_given_wins() {
        let (flags, rest) = take(&["--profile", "a", "browser", "--profile=b", "tabs"]);
        assert_eq!(flags.profile.as_deref(), Some("b"));
        assert_eq!(rest, strings(&["browser", "tabs"]));
    }

    #[test]
    fn a_profile_needs_a_name() {
        for given in [&["browser", "tabs", "--profile"][..], &["--profile"], &["--profile=", "browser"]] {
            let mut args = strings(given);
            assert!(take_browser_flags(&mut args, sidekar_command).is_err(), "{given:?}");
        }
    }

    #[test]
    fn no_browser_flags_leaves_argv_alone() {
        let (flags, rest) = take(&["kv", "get", "--profiles"]);
        assert_eq!(flags, BrowserFlags::default());
        assert_eq!(rest, strings(&["kv", "get", "--profiles"]));
    }

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
