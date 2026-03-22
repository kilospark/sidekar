//! PTY wrapper for launching and controlling owned agent sessions.
//!
//! `sidekar codex ...`, `sidekar claude ...`, etc. launch the agent inside
//! a sidekar-owned PTY. This gives us direct input injection (write to master fd),
//! signal forwarding, resize handling, and broker registration — all without tmux.

use crate::broker;
use crate::ipc;
use crate::message::AgentId;
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;

/// Known agent binary names.
pub const KNOWN_AGENTS: &[&str] = &[
    "codex", "claude", "gemini", "agent", "opencode", "copilot", "aider",
];

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

/// Write an entire buffer to a raw fd, retrying on short writes and EINTR.
/// Returns error on EAGAIN (non-blocking fd full) or other failures.
fn write_all_fd(fd: i32, mut buf: &[u8]) -> Result<()> {
    while !buf.is_empty() {
        let n = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
        if n > 0 {
            buf = &buf[n as usize..];
        } else if n == 0 {
            bail!("write returned 0");
        } else {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue; // EINTR — retry
            }
            bail!("write failed: {err}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Agent resolution
// ---------------------------------------------------------------------------

/// Resolve an agent name to its binary path. Returns a CString for execvp.
fn resolve_agent(agent: &str) -> Result<(String, std::ffi::CString)> {
    let output = std::process::Command::new("which")
        .arg(agent)
        .output()
        .with_context(|| format!("failed to look up \"{agent}\""))?;
    if !output.status.success() {
        bail!("\"{agent}\" not found on PATH. Is it installed?");
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let c_path = std::ffi::CString::new(path.as_str()).context("invalid binary path")?;
    Ok((path, c_path))
}

/// Build CString args for execvp (must happen before fork).
fn prepare_args(bin: &std::ffi::CString, args: &[String]) -> Result<Vec<std::ffi::CString>> {
    let mut c_args: Vec<std::ffi::CString> = vec![bin.clone()];
    for arg in args {
        c_args.push(std::ffi::CString::new(arg.as_str()).context("invalid arg")?);
    }
    Ok(c_args)
}

/// Detect a channel name: tmux session if available, otherwise project/hostname.
fn detect_channel() -> String {
    if let Some(pane) = ipc::detect_tmux_pane() {
        return pane.session;
    }
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
    let (bin_display, bin_c) = resolve_agent(agent)?;
    let c_args = prepare_args(&bin_c, args)?;
    eprintln!("sidekar pty: launching {agent} ({bin_display})");

    // Fork the child inside a PTY
    let (master, child_pid) = fork_pty(&bin_c, &c_args)?;
    let master_raw = master.as_raw_fd();

    // From here, any setup failure must clean up the child + broker + socket.
    // We track what we've registered so cleanup_child_and_state can tear it down.
    let mut registered_name: Option<String> = None;
    let mut socket_file: Option<std::path::PathBuf> = None;

    let setup_result = (|| -> Result<(Arc<OwnedFd>, AgentId)> {
        // Copy parent terminal size to child PTY
        let _ = copy_terminal_size(master_raw);

        // Set master fd to non-blocking for async I/O
        set_nonblocking(master_raw)?;

        // Build session identity with unique name
        let session_id = format!("pty-{child_pid}");
        let channel = detect_channel();
        let nick = crate::bus::pick_nickname_standalone();
        let name = unique_agent_name(agent, &channel);

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

        // Start IPC socket listener
        let master_arc = Arc::new(master);
        let path = ipc::start_socket_listener(
            &session_id,
            &session_id,
            &channel,
            &identity.name,
            Some(&nick),
            ipc::InputSink::PtyFd(master_arc.clone()),
        )?;
        broker::set_agent_socket_path(&identity.name, Some(path.as_path()))?;
        socket_file = Some(path);

        eprintln!(
            "sidekar pty: registered as \"{}\" aka \"{}\" on channel \"{}\"",
            identity.name, nick, channel
        );

        Ok((master_arc, identity))
    })();

    let (master_arc, identity) = match setup_result {
        Ok(v) => v,
        Err(e) => {
            eprintln!("sidekar pty: setup failed: {e}");
            cleanup_child_and_state(
                child_pid,
                registered_name.as_deref(),
                socket_file.as_deref(),
            );
            return Err(e);
        }
    };

    // Enter raw mode (must happen after eprintln messages)
    let raw_guard = RawModeGuard::enter()?;

    // Run the async event loop
    let exit_code = event_loop(&master_arc, child_pid).await;

    // Cleanup: restore terminal, unregister, remove socket
    drop(raw_guard);
    let _ = broker::unregister_agent(&identity.name);
    if let Some(ref path) = socket_file {
        let _ = std::fs::remove_file(path);
    }

    match exit_code {
        0 => Ok(()),
        code => std::process::exit(code),
    }
}

// ---------------------------------------------------------------------------
// Async event loop
// ---------------------------------------------------------------------------

async fn event_loop(master: &Arc<OwnedFd>, child_pid: libc::pid_t) -> i32 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::signal::unix::{SignalKind, signal};

    let master_fd = master.as_raw_fd();

    // Wrap master fd for async I/O
    let master_async = match tokio::io::unix::AsyncFd::new(master_fd) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("sidekar pty: AsyncFd failed: {e}");
            return 1;
        }
    };

    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    let mut sigwinch = signal(SignalKind::window_change()).unwrap();
    let mut sigterm = signal(SignalKind::terminate()).unwrap();

    let mut buf_in = [0u8; 4096];
    let mut buf_out = [0u8; 8192];

    loop {
        tokio::select! {
            biased;

            // SIGWINCH: resize child PTY
            _ = sigwinch.recv() => {
                let _ = copy_terminal_size(master_fd);
            }

            // SIGTERM: forward to child, exit
            _ = sigterm.recv() => {
                unsafe { libc::kill(child_pid, libc::SIGTERM) };
                break;
            }

            // stdin → master fd (user typing forwarded to agent)
            result = stdin.read(&mut buf_in) => {
                match result {
                    Ok(0) | Err(_) => break, // stdin closed
                    Ok(n) => {
                        if write_all_fd(master_fd, &buf_in[..n]).is_err() {
                            break;
                        }
                    }
                }
            }

            // master fd → stdout (agent output forwarded to terminal)
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
                                if stdout.write_all(&buf_out[..n]).await.is_err() {
                                    break;
                                }
                                let _ = stdout.flush().await;
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

    // Wait for child to exit
    let mut status: libc::c_int = 0;
    unsafe { libc::waitpid(child_pid, &mut status, 0) };

    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        1
    }
}
