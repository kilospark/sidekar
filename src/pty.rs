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

/// Shell-safe single-quote escaping: wraps in single quotes, escaping any
/// embedded single quotes as `'\''`.
fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    // If it's simple (no special chars), return as-is
    if s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'/') {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Check if a command can be resolved as an external agent (binary, alias, or function).
pub fn is_agent_command(command: &str) -> bool {
    resolve_agent(command).is_ok()
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
fn fork_pty(cmd: &std::ffi::CString, c_args: &[std::ffi::CString]) -> Result<(OwnedFd, libc::pid_t)> {
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

/// Copy the parent terminal size to the child PTY.
fn copy_terminal_size(master_fd: i32) -> Result<()> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ, &mut ws) } != 0 {
        return Ok(()); // not a terminal, skip
    }
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

/// Resolution result: either a direct binary path or a shell alias/function
/// that must be exec'd via the user's shell.
enum ResolvedAgent {
    /// Direct binary path (from `which`).
    Binary(String, std::ffi::CString),
    /// Shell alias or function — must exec via `$SHELL -ic '<command> ...'`.
    ShellAlias(String),
}

/// Resolve an agent name to its binary path, or detect that it's a shell alias/function.
fn resolve_agent(agent: &str) -> Result<ResolvedAgent> {
    // Try direct binary lookup first
    let output = std::process::Command::new("which")
        .arg(agent)
        .output()
        .with_context(|| format!("failed to look up \"{agent}\""))?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let c_path = std::ffi::CString::new(path.as_str()).context("invalid binary path")?;
        return Ok(ResolvedAgent::Binary(path, c_path));
    }

    // Not on PATH — check if the user's shell knows it (alias or function)
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let check = std::process::Command::new(&shell)
        .args(["-ic", &format!("type {agent} 2>/dev/null")])
        .output();
    if let Ok(out) = check {
        if out.status.success() {
            return Ok(ResolvedAgent::ShellAlias(shell));
        }
    }

    bail!("\"{agent}\" not found on PATH or as a shell alias/function. Is it installed?");
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
/// Channel can also be set at runtime via `@sidekar channel <name>`.
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
        .and_then(|p| p.rsplit('/').next().filter(|n| !n.is_empty()).map(|n| n.to_lowercase()))
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

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Launch an agent inside a sidekar-owned PTY.
pub async fn run_agent(agent: &str, args: &[String]) -> Result<()> {
    let (_bin_display, bin_c, c_args) = match resolve_agent(agent)? {
        ResolvedAgent::Binary(path, c_path) => {
            let c_args = prepare_args(&c_path, args)?;
            (path, c_path, c_args)
        }
        ResolvedAgent::ShellAlias(shell) => {
            // Build a single command string: "agent arg1 arg2 ..."
            // Use single-quote escaping for safety.
            let mut cmd_str = shell_quote(agent);
            for arg in args {
                cmd_str.push(' ');
                cmd_str.push_str(&shell_quote(arg));
            }
            let shell_c = std::ffi::CString::new(shell.as_str()).context("invalid shell path")?;
            let c_args = vec![
                shell_c.clone(),
                std::ffi::CString::new("-ic").unwrap(),
                std::ffi::CString::new(cmd_str.as_str()).context("invalid command string")?,
            ];
            (format!("{shell} -ic '{agent} ...'"), shell_c, c_args)
        }
    };

    // Build identity before fork so env vars are inherited by child.
    let channel = detect_channel();
    let nick = crate::bus::pick_nickname_standalone();
    let pre_fork_name = unique_agent_name(agent, &channel);

    // Set env vars before fork — child inherits them.
    // These let CLI commands (sidekar navigate, etc.) recover bus identity.
    // Safety: no other threads are reading these env vars at this point.
    unsafe {
        std::env::set_var("SIDEKAR_PTY", "1");
        std::env::set_var("SIDEKAR_AGENT_NAME", &pre_fork_name);
        std::env::set_var("SIDEKAR_CHANNEL", &channel);
    }

    // Fork the child inside a PTY
    let (master, child_pid) = fork_pty(&bin_c, &c_args)?;
    let master_raw = master.as_raw_fd();

    // Clear env vars in parent — they were only needed for the child.
    unsafe {
        std::env::remove_var("SIDEKAR_PTY");
        std::env::remove_var("SIDEKAR_AGENT_NAME");
        std::env::remove_var("SIDEKAR_CHANNEL");
    }

    // From here, any setup failure must clean up the child + broker.
    let mut registered_name: Option<String> = None;

    let setup_result = (|| -> Result<(Arc<OwnedFd>, AgentId, String)> {
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

        // Register with broker
        broker::register_agent(&identity, Some(&session_id))?;
        registered_name = Some(name.clone());

        // Start bus message poller (reads from SQLite, writes to PTY)
        let master_arc = Arc::new(master);
        crate::poller::start_poller(identity.name.clone(), master_arc.clone());

        Ok((master_arc, identity, nick))
    })();

    let (master_arc, identity, nick) = match setup_result {
        Ok(v) => v,
        Err(e) => {
            // silent — error propagated via return
            cleanup_child_and_state(
                child_pid,
                registered_name.as_deref(),
                None,
            );
            return Err(e);
        }
    };

    // Optionally establish tunnel to relay for web terminal access (dashboard / web terminal).
    let tunnel = if let Some(token) = crate::auth::auth_token() {
        let cwd_str = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".into());
        match crate::tunnel::connect(&token, &identity.name, agent, &cwd_str, &nick).await {
            Ok(t) => Some(t),
            Err(e) => {
                crate::broker::try_log_error_event("relay_tunnel", &format!("{e:#}"), None);
                None
            }
        }
    } else {
        crate::broker::try_log_error_event(
            "relay_tunnel",
            "skipped: no device token (~/.config/sidekar/auth.json); run: sidekar login",
            None,
        );
        None
    };

    // Ensure the Chrome extension bridge is running
    let _ = crate::ext::auto_launch_server();

    // Start the cron background loop (will pick up Chrome session when available)
    {
        let cron_ctx = crate::commands::cron::CronContext {
            cdp_port: crate::DEFAULT_CDP_PORT,
            cdp_host: crate::DEFAULT_CDP_HOST.to_string(),
            current_session_id: None,
            current_profile: "default".to_string(),
            headless: false,
            agent_name: Some(identity.name.clone()),
        };
        crate::commands::cron::start_cron_loop(cron_ctx).await;
    }

    // Start a background task to watch for the child's Chrome session.
    // When the child calls `sidekar launch` or `sidekar connect`, the
    // last-session file is updated. We read it and update the cron context.
    let session_watcher = tokio::spawn(watch_session_file(pre_fork_name.clone()));

    // Set terminal title to show agent nickname and name
    // OSC 0 sets both window title and icon name; works in all major terminals
    eprint!("\x1b]0;{} ({}) — {}\x07", nick, identity.name, agent);

    // Enter raw mode (must happen after eprintln messages)
    let raw_guard = RawModeGuard::enter()?;

    // Run the async event loop
    let exit_code = event_loop(&master_arc, child_pid, tunnel, &nick).await;

    // Cleanup: restore terminal, unregister, stop poller
    drop(raw_guard);

    // Reset terminal title
    eprint!("\x1b]0;\x07");

    session_watcher.abort();
    crate::poller::shutdown_poller();

    // Clean up Chrome resources owned by the child's session
    cleanup_chrome_session(&pre_fork_name).await;

    let _ = broker::unregister_agent(&identity.name);

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

/// Handle a `@sidekar <command>` line typed by the user.
/// Returns a response string to echo to the terminal.
fn handle_sidekar_command(line: &str) -> String {
    let parts: Vec<&str> = line.trim().splitn(3, ' ').collect();
    // parts[0] = "@sidekar", parts[1] = command, parts[2] = args
    let cmd = parts.get(1).map(|s| *s).unwrap_or("help");
    let arg = parts.get(2).map(|s| *s).unwrap_or("");

    match cmd {
        "channel" => {
            if arg.is_empty() {
                // Show current channel
                let channel = CHANNEL_OVERRIDE
                    .lock()
                    .ok()
                    .and_then(|g| g.clone())
                    .unwrap_or_else(detect_channel);
                format!("Channel: {channel}")
            } else {
                // Set channel
                if let Ok(mut guard) = CHANNEL_OVERRIDE.lock() {
                    *guard = Some(arg.to_string());
                }
                // Update broker registration
                if let Ok(agents) = broker::list_agents(None) {
                    let my_pid = std::process::id().to_string();
                    let my_pane = format!("pty-{my_pid}");
                    if let Some(agent) = agents.iter().find(|a| a.pane_unique_id.as_deref() == Some(&my_pane)) {
                        let updated = crate::message::AgentId {
                            name: agent.id.name.clone(),
                            nick: agent.id.nick.clone(),
                            session: Some(arg.to_string()),
                            pane: agent.id.pane.clone(),
                            agent_type: agent.id.agent_type.clone(),
                        };
                        let _ = broker::register_agent(&updated, Some(&my_pane));
                    }
                }
                format!("Channel set to: {arg}")
            }
        }
        "who" => {
            match broker::list_agents(None) {
                Ok(agents) if !agents.is_empty() => {
                    let mut lines = Vec::new();
                    for a in &agents {
                        let nick = a.id.nick.as_deref().unwrap_or("");
                        let chan = a.id.session.as_deref().unwrap_or("?");
                        lines.push(format!("  {} \"{}\" (channel: {})", a.id.name, nick, chan));
                    }
                    format!("Agents:\n{}", lines.join("\n"))
                }
                _ => "No agents registered.".to_string(),
            }
        }
        "cron" => {
            // Dispatch to cron subcommands synchronously via a small tokio block
            let sub_parts: Vec<&str> = arg.splitn(2, ' ').collect();
            let sub = sub_parts.first().copied().unwrap_or("list");
            match sub {
                "list" | "" => {
                    match crate::broker::list_cron_jobs(true) {
                        Ok(jobs) if jobs.is_empty() => "No active cron jobs.".to_string(),
                        Ok(jobs) => {
                            let mut lines = Vec::new();
                            for j in &jobs {
                                let name = j.name.as_deref().unwrap_or("(unnamed)");
                                lines.push(format!(
                                    "  [{}] {} — schedule: {} — target: {} — {} runs",
                                    j.id, name, j.schedule, j.target, j.run_count
                                ));
                            }
                            format!("Cron jobs:\n{}", lines.join("\n"))
                        }
                        Err(e) => format!("Error: {e}"),
                    }
                }
                "delete" => {
                    let job_id = sub_parts.get(1).copied().unwrap_or("").trim();
                    if job_id.is_empty() {
                        "@sidekar cron delete <job-id>".to_string()
                    } else {
                        match crate::broker::delete_cron_job(job_id) {
                            Ok(true) => format!("Cron job {job_id} deleted."),
                            Ok(false) => format!("Cron job '{job_id}' not found."),
                            Err(e) => format!("Error: {e}"),
                        }
                    }
                }
                _ => "@sidekar cron commands:\n  \
                      @sidekar cron list              — list active jobs\n  \
                      @sidekar cron delete <job-id>   — delete a job"
                    .to_string(),
            }
        }
        "help" | _ => {
            "@sidekar commands:\n  \
             @sidekar channel          — show current channel\n  \
             @sidekar channel <name>   — set channel\n  \
             @sidekar who              — list registered agents\n  \
             @sidekar cron list        — list cron jobs\n  \
             @sidekar cron delete <id> — delete a cron job\n  \
             @sidekar help             — show this help"
                .to_string()
        }
    }
}

/// Global channel override set by `@sidekar channel <name>`.
static CHANNEL_OVERRIDE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Get the effective channel: override → detect_channel().
#[allow(dead_code)]
pub(crate) fn effective_channel() -> String {
    CHANNEL_OVERRIDE
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(detect_channel)
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
) -> i32 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::signal::unix::{SignalKind, signal};

    // Prefix to prepend to any OSC title sequences from the child
    let nick_prefix = format!("{nick} — ");

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
            crate::broker::try_log_error_event(
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
            crate::broker::try_log_error_event(
                "signal",
                &format!("SIGTERM handler unavailable: {e}"),
                None,
            );
            None
        }
    };

    let mut buf_in = [0u8; 4096];
    let mut buf_out = [0u8; 8192];

    // Line buffer for @sidekar command interception.
    // Accumulates stdin bytes; when CR/LF is seen, checks for @sidekar prefix.
    let mut line_buf: Vec<u8> = Vec::with_capacity(256);
    let mut intercepting = false; // true when line_buf starts with "@sidekar"

    // Split tunnel into sender + receiver (if connected)
    let (tunnel_tx, mut tunnel_rx) = match tunnel {
        Some((tx, rx)) => (Some(tx), Some(rx)),
        None => (None, None),
    };

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
                        let _ = write_all_fd(master_fd, &data);
                    }
                    Some(crate::tunnel::TunnelEvent::Disconnected) => {}
                    None => {
                        tunnel_rx = None;
                    }
                }
            }

            // stdin → master fd (user typing forwarded to agent)
            // Intercepts lines starting with "@sidekar" for local commands.
            result = stdin.read(&mut buf_in) => {
                match result {
                    Ok(0) | Err(_) => break, // stdin closed
                    Ok(n) => {
                        for &byte in &buf_in[..n] {
                            if byte == b'\r' || byte == b'\n' {
                                if intercepting {
                                    // Handle the @sidekar command locally
                                    let line = String::from_utf8_lossy(&line_buf).to_string();
                                    let response = handle_sidekar_command(&line);
                                    // Echo newline + response to user's terminal
                                    let _ = stdout.write_all(b"\r\n").await;
                                    let _ = stdout.write_all(response.as_bytes()).await;
                                    let _ = stdout.write_all(b"\r\n").await;
                                    let _ = stdout.flush().await;
                                    line_buf.clear();
                                    intercepting = false;
                                } else {
                                    // Bytes were already forwarded individually;
                                    // just clear the prefix-detection buffer and
                                    // forward the CR/LF itself.
                                    line_buf.clear();
                                    let _ = write_all_fd(master_fd, &[byte]);
                                }
                            } else if byte == 0x7f || byte == 0x08 {
                                // Backspace — remove from buffer
                                if intercepting {
                                    line_buf.pop();
                                    // Echo backspace to terminal
                                    let _ = stdout.write_all(b"\x08 \x08").await;
                                    let _ = stdout.flush().await;
                                    // Check if we've backspaced out of @sidekar prefix
                                    if !line_buf.starts_with(b"@sidekar") {
                                        intercepting = false;
                                        // Flush what we have to the agent
                                        if !line_buf.is_empty() {
                                            let _ = write_all_fd(master_fd, &line_buf);
                                            line_buf.clear();
                                        }
                                    }
                                } else {
                                    // Forward backspace and update line buffer
                                    line_buf.pop();
                                    let _ = write_all_fd(master_fd, &[byte]);
                                }
                            } else {
                                line_buf.push(byte);
                                // Check if we're entering @sidekar mode
                                if line_buf.starts_with(b"@sidekar") {
                                    if !intercepting {
                                        intercepting = true;
                                    }
                                    // Echo character locally (agent won't see it)
                                    let _ = stdout.write_all(&[byte]).await;
                                    let _ = stdout.flush().await;
                                } else if intercepting {
                                    // We were intercepting but the prefix broke — shouldn't happen
                                    // since we check starts_with above, but safety net
                                    intercepting = false;
                                    let _ = write_all_fd(master_fd, &line_buf);
                                    line_buf.clear();
                                } else {
                                    // Normal byte — forward immediately
                                    let _ = write_all_fd(master_fd, &[byte]);
                                }
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
                                let raw = &buf_out[..n];
                                // Rewrite OSC title sequences to prepend nick
                                let data = rewrite_osc_titles(raw, &nick_prefix);
                                // Write to local stdout
                                if stdout.write_all(&data).await.is_err() {
                                    break;
                                }
                                let _ = stdout.flush().await;

                                // Fan-out to tunnel (non-blocking, best-effort) with raw bytes
                                if let Some(ref tx) = tunnel_tx {
                                    tx.send_data(raw.to_vec());
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
