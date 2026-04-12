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

mod chrome;
mod escape_filter;
mod event_loop;
mod identity;
mod session;

use chrome::{cleanup_chrome_session, watch_session_file};
use event_loop::event_loop;
use identity::{prepare_args, resolve_agent, unique_agent_name};
use session::{
    cleanup_child_and_state, connect_relay_tunnel, relay_policy_label, resolved_relay_policy,
};

type PtySetupState = (
    Arc<OwnedFd>,
    AgentId,
    String,
    String,
    Arc<crate::poller::UserInputState>,
);

// Re-export for external callers (e.g. bus.rs uses crate::pty::detect_channel)
pub(crate) use identity::detect_channel;

/// Check if a command is a known agent that sidekar should PTY-wrap.
pub fn is_agent_command(command: &str) -> bool {
    crate::agent_cli::is_pty_agent(command)
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

pub(super) fn current_terminal_size() -> Option<(u16, u16)> {
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
pub(super) fn copy_terminal_size(master_fd: i32) -> Result<()> {
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

fn wait_child_exit_or_terminate(child_pid: libc::pid_t) -> i32 {
    let mut status: libc::c_int = 0;
    for attempt in 0..20 {
        let waited = unsafe { libc::waitpid(child_pid, &mut status, libc::WNOHANG) };
        if waited == child_pid {
            return child_exit_code(status);
        }
        if waited < 0 {
            return 1;
        }
        if attempt == 0 {
            unsafe { libc::kill(child_pid, libc::SIGTERM) };
        } else if attempt == 5 {
            unsafe { libc::kill(child_pid, libc::SIGKILL) };
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let waited = unsafe { libc::waitpid(child_pid, &mut status, 0) };
    if waited == child_pid {
        child_exit_code(status)
    } else {
        1
    }
}

fn child_exit_code(status: libc::c_int) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        1
    }
}

/// Write an entire buffer to a raw fd, retrying on short writes, EINTR, and EAGAIN.
pub(super) fn write_all_fd(fd: i32, mut buf: &[u8]) -> Result<()> {
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
// Entry point
// ---------------------------------------------------------------------------

/// Launch an agent inside a sidekar-owned PTY.
pub async fn run_agent(
    agent: &str,
    args: &[String],
    relay_override: Option<bool>,
    proxy_override: Option<bool>,
) -> Result<()> {
    // Ensure rustls crypto provider is available before any WSS connection (relay tunnel).
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (path, c_path) = resolve_agent(agent)?;
    let enriched_args = crate::agent_cli::enrich_startup(agent, args);
    let c_args = prepare_args(&c_path, &enriched_args)?;
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
        let verbose = crate::runtime::verbose();
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
    let mut proxy_injected_codex_toml = false;
    if let Some((port, ref ca_path)) = proxy_info {
        let ca_str = ca_path.to_string_lossy().to_string();
        let (proxy_pairs, inject_codex) =
            crate::agent_cli::build_proxy_child_env(agent, port, &ca_str);
        for (k, v) in proxy_pairs {
            env_overrides.push((k, v));
        }
        if inject_codex {
            crate::proxy::inject_codex_ca(ca_path);
            proxy_injected_codex_toml = true;
        }
    }

    // Save originals, set overrides
    let saved_env: Vec<(&str, Option<String>)> = env_overrides
        .iter()
        .map(|(k, _)| (*k, std::env::var(k).ok()))
        .collect();
    // SAFETY (`env::set_var`): Temporary overrides for the child only; parent restores below
    // before continuing. No other thread should rely on these values during this window.
    unsafe {
        for (k, v) in &env_overrides {
            std::env::set_var(k, v);
        }
    }

    // Fork the child inside a PTY
    let (master, child_pid) = fork_pty(&bin_c, &c_args)?;
    let master_raw = master.as_raw_fd();

    // Restore parent env vars to their original state.
    // SAFETY: same as above — sequential restore before resuming normal execution.
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

    let setup_result = (|| -> Result<PtySetupState> {
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
        crate::poller::start_poller(
            identity.name.clone(),
            master_arc.clone(),
            input_state.clone(),
            child_pid,
        );

        Ok((master_arc, identity, nick, agent_session_id, input_state))
    })();

    let (master_arc, identity, nick, agent_session_id, input_state) = match setup_result {
        Ok(v) => v,
        Err(e) => {
            // silent — error propagated via return
            cleanup_child_and_state(child_pid, registered_name.as_deref(), None);
            if let Some((_, ref ca_path)) = proxy_info {
                crate::proxy::cleanup_ca_file(ca_path);
            }
            if proxy_injected_codex_toml {
                crate::proxy::remove_codex_ca();
            }
            return Err(e);
        }
    };

    if crate::runtime::verbose() {
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
            if crate::runtime::verbose() {
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
                    "skipped: no device token; run: sidekar device login",
                    Some(&relay_policy_text),
                );
                None
            }
        }
    };

    // Ensure the Chrome extension bridge / daemon is running
    let _ = crate::daemon::ensure_running();

    let pty_project = crate::scope::resolve_project_name(None);
    crate::commands::cron::start_default_cron_loop(identity.name.clone(), pty_project).await;

    // Start a background task to watch for the child's Chrome session.
    // When the child calls `sidekar launch` or `sidekar connect`, the
    // last-session file is updated. We read it and update the cron context.
    let session_watcher = tokio::spawn(watch_session_file(pre_fork_name.clone()));

    // Enter raw mode (must happen after eprintln messages)
    let raw_guard = match RawModeGuard::enter() {
        Ok(guard) => guard,
        Err(e) => {
            if let Some((ref tx, _)) = tunnel {
                tx.shutdown();
            }
            session_watcher.abort();
            crate::poller::shutdown_poller();
            cleanup_chrome_session(&pre_fork_name).await;
            if let Some((_, ref ca_path)) = proxy_info {
                crate::proxy::cleanup_ca_file(ca_path);
            }
            if proxy_injected_codex_toml {
                crate::proxy::remove_codex_ca();
            }
            let _ = broker::finish_agent_session(&agent_session_id, crate::message::epoch_secs());
            cleanup_child_and_state(child_pid, Some(&identity.name), None);
            return Err(e);
        }
    };

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

    // Remove injected proxy config from Codex config.toml (only if we injected)
    if proxy_injected_codex_toml {
        crate::proxy::remove_codex_ca();
    }

    let _ = broker::finish_agent_session(&agent_session_id, crate::message::epoch_secs());
    let _ = broker::unregister_agent(&identity.name);

    if crate::runtime::verbose() {
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
