//! PTY wrapper for launching and controlling owned agent sessions.
//!
//! `sidekar codex ...`, `sidekar claude ...`, etc. launch the agent inside
//! a sidekar-owned PTY. This gives us direct input injection (write to master fd),
//! signal forwarding, resize handling, and broker registration.

use crate::broker;

use crate::message::AgentId;
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;

/// Known agent executables that sidekar can PTY-wrap.
const KNOWN_AGENTS: &[&str] = &[
    "claude", "codex", "agent", "opencode", "pi", "gemini", "aider", "goose",
];

/// Check if a command is a known agent that sidekar should PTY-wrap.
pub fn is_agent_command(command: &str) -> bool {
    KNOWN_AGENTS.contains(&command)
}

// ---------------------------------------------------------------------------
// Terminal raw mode
// ---------------------------------------------------------------------------

/// RAII guard that restores terminal settings on drop.
struct RawModeGuard {
    saved: libc::termios,
    fd: i32,
}

impl RawModeGuard {
    fn enter() -> Result<Self> {
        let fd = libc::STDIN_FILENO;
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            bail!("tcgetattr failed: {}", std::io::Error::last_os_error());
        }
        let mut raw = saved;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            bail!("tcsetattr failed: {}", std::io::Error::last_os_error());
        }
        Ok(Self { saved, fd })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved) };
    }
}

// ---------------------------------------------------------------------------
// PTY operations
// ---------------------------------------------------------------------------

/// Fork a child process inside a new PTY.
///
/// The child side is async-signal-safe: CString args are prepared before
/// fork, and the child only calls execvp (or _exit on failure). No Rust
/// allocations, logging, or non-trivial work happens post-fork in the child.
fn fork_pty(
    cmd: &std::ffi::CString,
    c_args: &[std::ffi::CString],
) -> Result<(OwnedFd, libc::pid_t)> {
    // Build the argv pointer array before forking (allocation is safe here)
    let c_ptrs: Vec<*const libc::c_char> = c_args
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    let mut master_fd: libc::c_int = -1;
    let pid = unsafe {
        libc::forkpty(
            &mut master_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };

    if pid < 0 {
        bail!("forkpty failed: {}", std::io::Error::last_os_error());
    }

    if pid == 0 {
        // Child process — only async-signal-safe calls from here.
        // execvp replaces the process image; _exit if it fails.
        unsafe { libc::execvp(cmd.as_ptr(), c_ptrs.as_ptr()) };
        unsafe { libc::_exit(127) };
    }

    // Parent process
    let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
    Ok((master, pid))
}

fn current_terminal_size() -> Option<(u16, u16)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ, &mut ws) } != 0 {
        return None;
    }
    if ws.ws_col == 0 || ws.ws_row == 0 {
        return None;
    }
    Some((ws.ws_col, ws.ws_row))
}

/// Copy the parent terminal size to the child PTY.
fn copy_terminal_size(master_fd: i32) -> Result<()> {
    let Some((cols, rows)) = current_terminal_size() else {
        return Ok(());
    };
    let ws = libc::winsize {
        ws_col: cols,
        ws_row: rows,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe { libc::ioctl(master_fd, libc::TIOCSWINSZ, &ws) } != 0 {
        bail!("failed to set PTY window size");
    }
    Ok(())
}

/// Set an fd to non-blocking mode for async I/O.
fn set_nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        bail!("fcntl F_GETFL failed");
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        bail!("fcntl F_SETFL failed");
    }
    Ok(())
}

/// Write an entire buffer to a raw fd, retrying on short writes, EINTR, and EAGAIN.
fn write_all_fd(fd: i32, mut buf: &[u8]) -> Result<()> {
    let mut retries = 0u32;
    while !buf.is_empty() {
        let n = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
        if n > 0 {
            buf = &buf[n as usize..];
            retries = 0;
        } else if n == 0 {
            bail!("write returned 0");
        } else {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if err.kind() == std::io::ErrorKind::WouldBlock {
                retries += 1;
                if retries > 50 {
                    bail!("write to fd: buffer full after 50 retries");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
            bail!("write failed: {err}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Agent resolution
// ---------------------------------------------------------------------------

/// Resolve an agent name to its binary path via `which`.
fn resolve_agent(agent: &str) -> Result<(String, std::ffi::CString)> {
    let output = std::process::Command::new("which")
        .arg(agent)
        .output()
        .with_context(|| format!("failed to look up \"{agent}\""))?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let c_path = std::ffi::CString::new(path.as_str()).context("invalid binary path")?;
        return Ok((path, c_path));
    }
    bail!("\"{agent}\" not found on PATH. Is it installed?");
}

/// Build CString args for execvp (must happen before fork).
fn prepare_args(bin: &std::ffi::CString, args: &[String]) -> Result<Vec<std::ffi::CString>> {
    let mut c_args: Vec<std::ffi::CString> = vec![bin.clone()];
    for arg in args {
        c_args.push(std::ffi::CString::new(arg.as_str()).context("invalid arg")?);
    }
    Ok(c_args)
}

/// Detect a channel name. Priority: $PWD → git repo name → hostname.
pub(crate) fn detect_channel() -> String {
    // 1. Full path ($PWD) — agents in the same directory are on the same channel
    if let Ok(cwd) = std::env::current_dir() {
        return cwd.to_string_lossy().to_string();
    }
    // 2. Git repo name
    if let Some(name) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .and_then(|p| {
            p.rsplit('/')
                .next()
                .filter(|n| !n.is_empty())
                .map(|n| n.to_lowercase())
        })
    {
        return name;
    }
    // 3. Hostname
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "local".into())
}

/// Pick a unique agent name like `{agent}-{channel}-{n}`, checking the broker
/// for existing names to avoid collisions.
fn unique_agent_name(agent: &str, channel: &str) -> String {
    let mut existing: HashSet<String> = HashSet::new();
    if let Ok(agents) = broker::list_agents(None) {
        for a in agents {
            existing.insert(a.id.name);
        }
    }
    let mut n = 1u32;
    loop {
        let candidate = format!("{agent}-{channel}-{n}");
        if !existing.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

// ---------------------------------------------------------------------------
// Cleanup helper
// ---------------------------------------------------------------------------

/// Kill and reap a child process, clean up broker and socket state.
fn cleanup_child_and_state(
    child_pid: libc::pid_t,
    agent_name: Option<&str>,
    socket_path: Option<&std::path::Path>,
) {
    // Kill child if still running
    if unsafe { libc::kill(child_pid, 0) } == 0 {
        unsafe { libc::kill(child_pid, libc::SIGTERM) };
        // Brief wait, then force kill
        std::thread::sleep(std::time::Duration::from_millis(500));
        if unsafe { libc::kill(child_pid, 0) } == 0 {
            unsafe { libc::kill(child_pid, libc::SIGKILL) };
        }
    }
    // Reap
    let mut status: libc::c_int = 0;
    unsafe { libc::waitpid(child_pid, &mut status, libc::WNOHANG) };

    // Clean broker
    if let Some(name) = agent_name {
        let _ = broker::unregister_agent(name);
    }
    // Clean socket
    if let Some(path) = socket_path {
        let _ = std::fs::remove_file(path);
    }
}

fn resolved_relay_policy(override_policy: Option<bool>) -> crate::config::RelayMode {
    match override_policy {
        Some(true) => crate::config::RelayMode::On,
        Some(false) => crate::config::RelayMode::Off,
        None => crate::config::relay_mode(),
    }
}

fn relay_policy_label(override_policy: Option<bool>) -> String {
    match override_policy {
        Some(true) => "--relay".to_string(),
        Some(false) => "--no-relay".to_string(),
        None => format!("config:{}", crate::config::relay_mode().as_str()),
    }
}

async fn connect_relay_tunnel(
    token: &str,
    session_name: &str,
    agent_type: &str,
    cwd: &str,
    nick: &str,
) -> Result<(crate::tunnel::TunnelSender, crate::tunnel::TunnelReceiver)> {
    let (cols, rows) = current_terminal_size().unwrap_or((80, 24));
    crate::tunnel::connect(token, session_name, agent_type, cwd, nick, cols, rows).await
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// First message injected into the agent's stdin after startup.
/// Compact directive: load sidekar, set behavioral ground rules.
const STARTUP_INJECT: &str =
    "load sidekar skill. never guess or assume. verify in source — docs can be stale. ask if unclear. no sycophancy — think critically. no shortcuts or quickfixes — find the root cause.";

/// How long the agent must be quiet (no output) before we inject the startup message.
const STARTUP_INJECT_QUIET_MS: u64 = 1500;
/// Minimum time before injection — don't inject during early boot.
const STARTUP_INJECT_MIN_SECS: u64 = 5;
/// If idle detection hasn't fired by this point, inject anyway (TUI apps may never go quiet).
const STARTUP_INJECT_FALLBACK_SECS: u64 = 15;

/// Launch an agent inside a sidekar-owned PTY.
pub async fn run_agent(agent: &str, args: &[String], relay_override: Option<bool>, proxy_override: Option<bool>) -> Result<()> {
    let (path, c_path) = resolve_agent(agent)?;
    let c_args = prepare_args(&c_path, args)?;
    let bin_display = path;
    let bin_c = c_path;

    // Build identity before fork so env vars are inherited by child.
    let channel = detect_channel();
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let nick = crate::bus::pick_nickname_for_project(Some(&cwd));
    let pre_fork_name = unique_agent_name(agent, &channel);
    let start_time = std::time::Instant::now();
    let started_at = crate::message::epoch_secs();

    // Optionally start MITM proxy before fork so the child inherits env vars.
    let proxy_enabled = match proxy_override {
        Some(v) => v,
        None => std::env::var("SIDEKAR_PROXY").is_ok(),
    };
    let proxy_info = if proxy_enabled {
        let verbose = std::env::var("SIDEKAR_VERBOSE").is_ok();
        match crate::proxy::start(verbose).await {
            Ok((port, ca_path)) => Some((port, ca_path)),
            Err(e) => {
                crate::broker::try_log_error("proxy", &format!("failed to start: {e:#}"), None);
                None
            }
        }
    } else {
        None
    };

    // Save, set, fork, restore env vars — child inherits them via fork.
    // Safety: no other threads are reading these env vars at this point.
    let mut env_overrides: Vec<(&str, String)> = vec![
        ("SIDEKAR_PTY", "1".into()),
        ("SIDEKAR_AGENT_NAME", pre_fork_name.clone()),
        ("SIDEKAR_CHANNEL", channel.clone()),
    ];
    if let Some((port, ref ca_path)) = proxy_info {
        let base = format!("http://127.0.0.1:{port}");
        let ca_str = ca_path.to_string_lossy().to_string();
        for var in crate::proxy::PROXY_ENV_VARS {
            let val = match *var {
                "ANTHROPIC_BASE_URL" => base.clone(),
                "HTTPS_PROXY" | "https_proxy" => base.clone(),
                "NO_PROXY" | "no_proxy" => "127.0.0.1,localhost".into(),
                _ => ca_str.clone(), // cert paths (NODE_EXTRA_CA_CERTS, SSL_CERT_FILE, CODEX_CA_CERTIFICATE)
            };
            env_overrides.push((var, val));
        }
        // Inject ca-certificate into Codex config.toml (cleaned up on exit)
        crate::proxy::inject_codex_ca(ca_path);
    }

    // Save originals, set overrides
    let saved_env: Vec<(&str, Option<String>)> = env_overrides
        .iter()
        .map(|(k, _)| (*k, std::env::var(k).ok()))
        .collect();
    unsafe {
        for (k, v) in &env_overrides {
            std::env::set_var(k, v);
        }
    }

    // Fork the child inside a PTY
    let (master, child_pid) = fork_pty(&bin_c, &c_args)?;
    let master_raw = master.as_raw_fd();

    // Restore parent env vars to their original state.
    unsafe {
        for (k, original) in &saved_env {
            match original {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    // From here, any setup failure must clean up the child + broker.
    let mut registered_name: Option<String> = None;

    let setup_result = (|| -> Result<(
        Arc<OwnedFd>,
        AgentId,
        String,
        String,
        Arc<crate::poller::UserInputState>,
    )> {
        // Copy parent terminal size to child PTY
        let _ = copy_terminal_size(master_raw);

        // Set master fd to non-blocking for async I/O
        set_nonblocking(master_raw)?;

        // Build session identity with unique name
        let session_id = format!("pty-{child_pid}");
        let name = pre_fork_name.clone();

        let identity = AgentId {
            name: name.clone(),
            nick: Some(nick.clone()),
            session: Some(channel.clone()),
            pane: Some(session_id.clone()),
            agent_type: Some("sidekar".into()),
        };
        let agent_session_id = format!("pty:{child_pid}:{started_at}");

        // Register with broker
        broker::register_agent(&identity, Some(&session_id))?;
        registered_name = Some(name.clone());
        broker::create_agent_session(
            &agent_session_id,
            &identity.name,
            Some(agent),
            identity.nick.as_deref(),
            &cwd,
            identity.session.as_deref(),
            Some(&cwd),
            started_at,
        )?;

        // Start bus message poller (reads from SQLite, writes to PTY)
        let master_arc = Arc::new(master);
        let input_state = Arc::new(crate::poller::UserInputState::default());
        crate::poller::start_poller(identity.name.clone(), master_arc.clone(), input_state.clone());

        Ok((master_arc, identity, nick, agent_session_id, input_state))
    })();

    let (master_arc, identity, nick, agent_session_id, input_state) = match setup_result {
        Ok(v) => v,
        Err(e) => {
            // silent — error propagated via return
            cleanup_child_and_state(child_pid, registered_name.as_deref(), None);
            return Err(e);
        }
    };

    let _ = crate::memory::start_agent_session(&identity.name, &cwd);
    if std::env::var("SIDEKAR_VERBOSE").is_ok() {
        crate::broker::try_log_event(
            "debug",
            "pty",
            "agent_launch",
            Some(&format!(
                "agent={agent} command={} args={} relay_policy={}",
                bin_display,
                args.join(" "),
                relay_policy_label(relay_override),
            )),
        );
    }

    let relay_policy = resolved_relay_policy(relay_override);
    let relay_policy_text = relay_policy_label(relay_override);

    // Optionally establish tunnel to relay for web terminal access (dashboard / web terminal).
    let tunnel = match relay_policy {
        crate::config::RelayMode::Off | crate::config::RelayMode::Auto => {
            if std::env::var("SIDEKAR_VERBOSE").is_ok() {
                crate::broker::try_log_event(
                    "debug",
                    "relay",
                    "disabled by policy",
                    Some(&relay_policy_text),
                );
            }
            None
        }
        crate::config::RelayMode::On => {
            if let Some(token) = crate::auth::auth_token() {
                match connect_relay_tunnel(&token, &identity.name, agent, &cwd, &nick).await {
                    Ok(t) => Some(t),
                    Err(e) => {
                        crate::broker::try_log_error(
                            "relay",
                            &format!("{e:#}"),
                            Some(&relay_policy_text),
                        );
                        None
                    }
                }
            } else {
                crate::broker::try_log_error(
                    "relay",
                    "skipped: no device token; run: sidekar login",
                    Some(&relay_policy_text),
                );
                None
            }
        }
    };

    // Ensure the Chrome extension bridge / daemon is running
    let _ = crate::daemon::ensure_running();

    crate::commands::cron::start_default_cron_loop(identity.name.clone()).await;

    // Start a background task to watch for the child's Chrome session.
    // When the child calls `sidekar launch` or `sidekar connect`, the
    // last-session file is updated. We read it and update the cron context.
    let session_watcher = tokio::spawn(watch_session_file(pre_fork_name.clone()));

    // Build startup message: directives + memory context
    let startup_msg = {
        let mut msg = STARTUP_INJECT.to_string();
        if let Ok(brief) = crate::memory::startup_brief(3) {
            let brief = brief.trim();
            if !brief.is_empty() {
                msg.push_str("\n\n[memory context]\n");
                msg.push_str(brief);
            }
        }
        msg
    };

    // Inject startup message once the agent is idle (produced output then went quiet).
    // Skipped if the user types anything first.
    {
        let inject_fd = master_arc.clone();
        let inject_input = input_state.clone();
        tokio::spawn(async move {
            let start = tokio::time::Instant::now();
            let min_wait = std::time::Duration::from_secs(STARTUP_INJECT_MIN_SECS);
            let fallback = start + std::time::Duration::from_secs(STARTUP_INJECT_FALLBACK_SECS);
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                if inject_input.has_ever_had_input() || inject_input.has_pending_line() {
                    return; // user typed first — skip
                }
                let elapsed = start.elapsed();
                if elapsed >= min_wait && inject_input.agent_idle_for(STARTUP_INJECT_QUIET_MS) {
                    break; // agent booted and went quiet — ready
                }
                if tokio::time::Instant::now() >= fallback {
                    break; // fallback for TUI apps that never go fully quiet
                }
            }
            // Flatten to single line — multiline pastes confuse some TUI editors
            let flat = startup_msg.replace('\n', " ");
            let fd = inject_fd.as_raw_fd();
            let _ = write_all_fd(fd, flat.as_bytes());
            std::thread::sleep(std::time::Duration::from_millis(200));
            // Send both CR and LF — different TUI frameworks use different submit keys
            let _ = write_all_fd(fd, b"\r\n");
        });
    }

    // Enter raw mode (must happen after eprintln messages)
    let raw_guard = RawModeGuard::enter()?;

    // Run the async event loop
    let exit_code = event_loop(
        &master_arc,
        child_pid,
        tunnel,
        &nick,
        &identity.name,
        &input_state,
    )
    .await;

    // Clean up proxy CA file
    if let Some((_, ref ca_path)) = proxy_info {
        crate::proxy::cleanup_ca_file(ca_path);
    }

    // Cleanup: restore terminal, unregister, stop poller
    drop(raw_guard);

    session_watcher.abort();
    crate::poller::shutdown_poller();

    // Clean up Chrome resources owned by the child's session
    cleanup_chrome_session(&pre_fork_name).await;

    // Remove injected proxy config from Codex config.toml
    if proxy_info.is_some() {
        crate::proxy::remove_codex_ca();
    }

    let _ = crate::memory::finish_agent_session(&identity.name);
    let _ = broker::finish_agent_session(&agent_session_id, crate::message::epoch_secs());
    let _ = broker::unregister_agent(&identity.name);

    if std::env::var("SIDEKAR_VERBOSE").is_ok() {
        crate::broker::try_log_event(
            "debug",
            "pty",
            "agent_exit",
            Some(&format!(
                "agent={} exit_code={} runtime_ms={}",
                agent,
                exit_code,
                start_time.elapsed().as_millis()
            )),
        );
    }

    // process::exit to terminate the poller thread immediately
    std::process::exit(exit_code);
}

// ---------------------------------------------------------------------------
// Session file watcher — picks up Chrome session from child agent
// ---------------------------------------------------------------------------

/// Watch the per-agent session file for changes. When the child agent calls
/// `sidekar launch` or `sidekar connect`, this file is updated with the session ID.
/// We read the session state to get CDP port and update the cron context.
async fn watch_session_file(agent_name: String) {
    use tokio::time::{Duration, interval};

    let data_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".sidekar");
    let safe_name = crate::sanitize_for_filename(&agent_name);
    let last_session_file = data_dir.join(format!("last-session-{safe_name}"));

    let mut poll = interval(Duration::from_secs(2));
    let mut last_contents = String::new();

    loop {
        poll.tick().await;

        let contents = match std::fs::read_to_string(&last_session_file) {
            Ok(c) => c.trim().to_string(),
            Err(_) => continue,
        };

        if contents == last_contents || contents.is_empty() {
            continue;
        }
        last_contents = contents.clone();

        // Read session state to get CDP port
        let state_file = data_dir.join(format!("state-{contents}.json"));
        if let Ok(state_str) = std::fs::read_to_string(&state_file) {
            if let Ok(state) = serde_json::from_str::<serde_json::Value>(&state_str) {
                let port = state.get("port").and_then(|v| v.as_u64()).unwrap_or(9222) as u16;
                let host = state
                    .get("host")
                    .and_then(|v| v.as_str())
                    .unwrap_or("127.0.0.1")
                    .to_string();
                let cron_ctx = crate::commands::cron::CronContext {
                    cdp_port: port,
                    cdp_host: host,
                    current_session_id: Some(contents.clone()),
                    current_profile: state
                        .get("profile")
                        .and_then(|v| v.as_str())
                        .unwrap_or("default")
                        .to_string(),
                    headless: false,
                    agent_name: Some(agent_name.clone()),
                };
                crate::commands::cron::update_cron_context(cron_ctx).await;
                // silent — don't print to the pty terminal
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Chrome cleanup — close tabs/windows owned by the child's session
// ---------------------------------------------------------------------------

/// Close Chrome tabs and windows owned by the child's last session.
async fn cleanup_chrome_session(agent_name: &str) {
    let data_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".sidekar");
    let safe_name = crate::sanitize_for_filename(agent_name);
    let last_session_file = data_dir.join(format!("last-session-{safe_name}"));

    let session_id = match std::fs::read_to_string(&last_session_file) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return,
    };
    if session_id.is_empty() {
        return;
    }

    let state_file = data_dir.join(format!("state-{session_id}.json"));
    let state: crate::types::SessionState = match std::fs::read_to_string(&state_file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(s) => s,
        None => return,
    };

    let port = state.port.unwrap_or(9222);
    let host = state.host.as_deref().unwrap_or("127.0.0.1");
    let base_url = format!("http://{host}:{port}");

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };

    // Close all tabs owned by this session
    for tab_id in &state.tabs {
        let _ = client
            .put(format!("{base_url}/json/close/{tab_id}"))
            .send()
            .await;
    }

    // Clean up session state file and per-agent session pointer
    let _ = std::fs::remove_file(&state_file);
    let _ = std::fs::remove_file(&last_session_file);
}

// ---------------------------------------------------------------------------
// Async event loop
// ---------------------------------------------------------------------------

/// Filter out OSC color query/response sequences (OSC 10-19) from input data.
/// These are terminal color queries (ESC ] 10 ; ? BEL) and responses (ESC ] 10 ; rgb:... BEL)
/// that can leak through when the browser's xterm.js queries colors or when the host
/// terminal responds to queries. Filtering prevents them from appearing as literal text.
fn filter_osc_color_sequences(data: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    // Fast path: no ESC in data
    if !data.contains(&0x1b) {
        return std::borrow::Cow::Borrowed(data);
    }

    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;

    while i < data.len() {
        // Look for ESC ] (0x1b 0x5d) - start of OSC sequence
        if data[i] == 0x1b && i + 1 < data.len() && data[i + 1] == 0x5d {
            // Check for OSC 10-19 (color-related sequences)
            if i + 2 < data.len() && data[i + 2] == b'1' {
                // OSC 10-19: look for the next digit
                let is_color_osc = if i + 3 < data.len() {
                    let d = data[i + 3];
                    // OSC 10, 11, 12, ... 19 followed by ; or terminator
                    d == b';'
                        || d == b'0'
                        || d == b'1'
                        || d == b'2'
                        || d == b'3'
                        || d == b'4'
                        || d == b'5'
                        || d == b'6'
                        || d == b'7'
                        || d == b'8'
                        || d == b'9'
                        || d == 0x07
                } else {
                    false
                };

                if is_color_osc {
                    // Skip the entire OSC sequence until BEL or ST
                    let mut j = i + 2;
                    while j < data.len() {
                        if data[j] == 0x07 {
                            i = j + 1;
                            break;
                        }
                        if data[j] == 0x1b && j + 1 < data.len() && data[j + 1] == b'\\' {
                            i = j + 2;
                            break;
                        }
                        j += 1;
                    }
                    if j >= data.len() {
                        // Unterminated sequence, skip rest
                        i = data.len();
                    }
                    continue;
                }
            }
        }

        out.push(data[i]);
        i += 1;
    }

    if out.len() == data.len() {
        std::borrow::Cow::Borrowed(data)
    } else {
        std::borrow::Cow::Owned(out)
    }
}

/// Rewrite OSC 0/2 title sequences (ESC ] 0; ... BEL / ESC ] 2; ... BEL)
/// to prepend the nick prefix, so the terminal title always shows the agent nickname.
fn rewrite_osc_titles<'a>(data: &'a [u8], prefix: &str) -> std::borrow::Cow<'a, [u8]> {
    // Fast path: no ESC in data, nothing to rewrite
    if !data.contains(&0x1b) {
        return std::borrow::Cow::Borrowed(data);
    }

    let mut out = Vec::with_capacity(data.len() + 64);
    let mut i = 0;

    while i < data.len() {
        // Look for ESC ] (0x1b 0x5d)
        if data[i] == 0x1b && i + 1 < data.len() && data[i + 1] == 0x5d {
            // Check for OSC 0; or OSC 2; (set window title)
            if i + 3 < data.len()
                && (data[i + 2] == b'0' || data[i + 2] == b'2')
                && data[i + 3] == b';'
            {
                // Find the terminator: BEL (0x07) or ST (ESC \)
                let start = i + 4; // start of title text
                let mut end = start;
                while end < data.len() {
                    if data[end] == 0x07 {
                        break;
                    }
                    if data[end] == 0x1b && end + 1 < data.len() && data[end + 1] == b'\\' {
                        break;
                    }
                    end += 1;
                }

                if end < data.len() {
                    // Write rewritten OSC: ESC ] <digit> ; <prefix><original title> <terminator>
                    out.push(0x1b);
                    out.push(0x5d);
                    out.push(data[i + 2]); // '0' or '2'
                    out.push(b';');
                    out.extend_from_slice(prefix.as_bytes());
                    out.extend_from_slice(&data[start..end]);

                    if data[end] == 0x07 {
                        out.push(0x07);
                        i = end + 1;
                    } else {
                        // ST: ESC backslash
                        out.push(0x1b);
                        out.push(b'\\');
                        i = end + 2;
                    }
                    continue;
                }
            }
        }

        out.push(data[i]);
        i += 1;
    }

    std::borrow::Cow::Owned(out)
}

async fn event_loop(
    master: &Arc<OwnedFd>,
    child_pid: libc::pid_t,
    tunnel: Option<(crate::tunnel::TunnelSender, crate::tunnel::TunnelReceiver)>,
    nick: &str,
    agent_name: &str,
    input_state: &Arc<crate::poller::UserInputState>,
) -> i32 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::signal::unix::{SignalKind, signal};

    let nick_prefix = if nick.is_empty() {
        String::new()
    } else {
        format!("{nick} — ")
    };

    let master_fd = master.as_raw_fd();

    // Wrap master fd for async I/O
    let master_async = match tokio::io::unix::AsyncFd::new(master_fd) {
        Ok(fd) => fd,
        Err(_e) => {
            // silent — error code returned
            return 1;
        }
    };

    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    // Signal registration can fail (FD limits, sandbox). Do not panic — abort would kill the PTY wrapper.
    let mut sigwinch = match signal(SignalKind::window_change()) {
        Ok(s) => Some(s),
        Err(e) => {
            crate::broker::try_log_error(
                "signal",
                &format!("SIGWINCH handler unavailable: {e}"),
                None,
            );
            None
        }
    };
    let mut sigterm_sig = match signal(SignalKind::terminate()) {
        Ok(s) => Some(s),
        Err(e) => {
            crate::broker::try_log_error(
                "signal",
                &format!("SIGTERM handler unavailable: {e}"),
                None,
            );
            None
        }
    };

    let mut buf_in = [0u8; 4096];
    let mut buf_out = [0u8; 8192];

    // Line buffer for pending-user-input tracking.
    let mut line_buf: Vec<u8> = Vec::with_capacity(256);
    // Split tunnel into sender + receiver (if connected)
    let (tunnel_tx, mut tunnel_rx) = match tunnel {
        Some((tx, rx)) => (Some(tx), Some(rx)),
        None => (None, None),
    };

    // Structured event parser — emits semantic events alongside raw PTY bytes
    let mut event_parser = crate::events::EventParser::new();

    loop {
        tokio::select! {
            biased;

            // SIGWINCH: resize child PTY
            _ = async {
                match &mut sigwinch {
                    Some(s) => s.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                let _ = copy_terminal_size(master_fd);
                if let (Some(tx), Some((cols, rows))) = (tunnel_tx.as_ref(), current_terminal_size()) {
                    tx.send_terminal_resize(cols, rows);
                }
            }

            // SIGTERM: forward to child, exit
            _ = async {
                match &mut sigterm_sig {
                    Some(s) => s.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                unsafe { libc::kill(child_pid, libc::SIGTERM) };
                break;
            }

            // Tunnel → master fd (browser input injected into agent)
            event = async {
                match tunnel_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match event {
                    Some(crate::tunnel::TunnelEvent::Data(data)) => {
                        // Filter out OSC color queries from browser's xterm.js
                        let filtered = filter_osc_color_sequences(&data);
                        let _ = write_all_fd(master_fd, &filtered);
                    }
                    Some(crate::tunnel::TunnelEvent::BusRelay {
                        recipient,
                        sender,
                        body,
                        envelope,
                    }) => {
                        if recipient == agent_name {
                            if let Some(envelope) = envelope {
                                match envelope.kind {
                                    crate::message::MessageKind::Request
                                    | crate::message::MessageKind::Handoff => {
                                        let _ = crate::broker::set_pending(&envelope);
                                    }
                                    crate::message::MessageKind::Response => {
                                        if let Some(reply_to) = envelope.reply_to.as_deref() {
                                            let _ = crate::broker::record_reply(reply_to, &envelope);
                                        }
                                    }
                                    crate::message::MessageKind::Fyi => {}
                                }
                            }
                            let _ = crate::broker::enqueue_message(&sender, &recipient, &body);
                        }
                    }
                    Some(crate::tunnel::TunnelEvent::BusPlain(body)) => {
                        let _ = write_all_fd(master_fd, body.as_bytes());
                        let _ = write_all_fd(master_fd, b"\r\n");
                    }
                    Some(crate::tunnel::TunnelEvent::Disconnected) => {}
                    None => {
                        tunnel_rx = None;
                    }
                }
            }

            // stdin → master fd (user typing forwarded to agent)
            result = stdin.read(&mut buf_in) => {
                match result {
                    Ok(0) | Err(_) => break, // stdin closed
                    Ok(n) => {
                        let chunk = &buf_in[..n];

                        // For local PTY sessions, pass terminal control replies through unchanged.
                        // Codex probes the terminal on startup and expects the real terminal's
                        // responses back on stdin. Swallowing those breaks its renderer.
                        // Don't mark as user activity — these are terminal auto-replies,
                        // not real user input.
                        if chunk.contains(&0x1b) {
                            let _ = write_all_fd(master_fd, chunk);
                            continue;
                        }

                        input_state.mark_activity();

                        for &byte in chunk {
                            if byte == b'\r' || byte == b'\n' {
                                line_buf.clear();
                                input_state.clear_pending_line();
                                let _ = write_all_fd(master_fd, &[byte]);
                            } else if byte == 0x7f || byte == 0x08 {
                                line_buf.pop();
                                input_state.set_pending_line(&line_buf);
                                let _ = write_all_fd(master_fd, &[byte]);
                            } else {
                                line_buf.push(byte);
                                input_state.set_pending_line(&line_buf);
                                let _ = write_all_fd(master_fd, &[byte]);
                            }
                        }
                    }
                }
            }

            // master fd → stdout AND tunnel (agent output)
            result = master_async.readable() => {
                match result {
                    Ok(mut guard) => {
                        match guard.try_io(|_| {
                            let n = unsafe {
                                libc::read(master_fd, buf_out.as_mut_ptr() as *mut libc::c_void, buf_out.len())
                            };
                            if n > 0 {
                                Ok(n as usize)
                            } else if n == 0 {
                                Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "child exited"))
                            } else {
                                Err(std::io::Error::last_os_error())
                            }
                        }) {
                            Ok(Ok(n)) => {
                                input_state.mark_agent_output();
                                let raw = &buf_out[..n];
                                // Preserve terminal transparency except for OSC window-title
                                // sequences, where we prefix the agent nickname.
                                let local_data = if nick_prefix.is_empty() {
                                    std::borrow::Cow::Borrowed(raw)
                                } else {
                                    rewrite_osc_titles(raw, &nick_prefix)
                                };
                                if stdout.write_all(&local_data).await.is_err() {
                                    break;
                                }
                                let _ = stdout.flush().await;

                                // Fan-out to tunnel with normalized control sequences for the web terminal.
                                if let Some(ref tx) = tunnel_tx {
                                    let filtered = filter_osc_color_sequences(raw);
                                    let tunnel_data = if nick_prefix.is_empty() {
                                        filtered.into_owned()
                                    } else {
                                        rewrite_osc_titles(&filtered, &nick_prefix).into_owned()
                                    };
                                    tx.send_data(tunnel_data);

                                    // Emit structured events alongside raw bytes
                                    for event in event_parser.feed(raw) {
                                        tx.send_event(crate::events::event_to_json(&event));
                                    }
                                }
                            }
                            Ok(Err(_)) => break,
                            Err(_would_block) => continue,
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    // Flush the async stdout — process::exit() won't run Drop impls, and
    // the tokio stdout has its own buffer separate from std::io::stdout().
    // The child's final escape sequences (rmcup etc.) must be flushed now.
    let _ = stdout.flush().await;

    // Flush any pending events before shutting down
    if let Some(ref tx) = tunnel_tx {
        for event in event_parser.flush() {
            tx.send_event(crate::events::event_to_json(&event));
        }
    }

    // Shut down tunnel gracefully
    if let Some(tx) = tunnel_tx {
        tx.shutdown();
    }

    // Wait for child to exit
    let mut status: libc::c_int = 0;
    unsafe { libc::waitpid(child_pid, &mut status, 0) };

    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn rewrite_osc_titles_prepends_formatted_nick_prefix() {
        let raw = b"\x1b]0;claude\x07";
        let rewritten = rewrite_osc_titles(raw, "borzoi — ");
        assert_eq!(rewritten.as_ref(), b"\x1b]0;borzoi \xE2\x80\x94 claude\x07");
    }
}
