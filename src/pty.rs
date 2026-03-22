//! PTY wrapper for launching and controlling owned agent sessions.
//!
//! `sidekar codex ...`, `sidekar claude ...`, etc. launch the agent inside
//! a sidekar-owned PTY. This gives us direct input injection (write to master fd),
//! signal forwarding, resize handling, and broker registration — all without tmux.

use crate::broker;
use crate::ipc;
use crate::message::AgentId;
use anyhow::{Context, Result, bail};
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

    fn restore(&self) {
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved) };
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

// ---------------------------------------------------------------------------
// PTY operations
// ---------------------------------------------------------------------------

/// Fork a child process inside a new PTY.
/// Returns (master_fd, child_pid) in the parent. Does not return in the child.
fn fork_pty(cmd: &str, args: &[String]) -> Result<(OwnedFd, libc::pid_t)> {
    let mut master_fd: libc::c_int = -1;
    let pid = unsafe { libc::forkpty(&mut master_fd, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };

    if pid < 0 {
        bail!("forkpty failed: {}", std::io::Error::last_os_error());
    }

    if pid == 0 {
        // Child process — exec the agent
        let c_cmd = std::ffi::CString::new(cmd).context("invalid command")?;
        let mut c_args: Vec<std::ffi::CString> = vec![c_cmd.clone()];
        for arg in args {
            c_args.push(std::ffi::CString::new(arg.as_str()).context("invalid arg")?);
        }
        let c_ptrs: Vec<*const libc::c_char> = c_args
            .iter()
            .map(|s| s.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect();
        unsafe { libc::execvp(c_cmd.as_ptr(), c_ptrs.as_ptr()) };
        // If execvp returns, it failed
        eprintln!("sidekar: exec failed for \"{cmd}\": {}", std::io::Error::last_os_error());
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

/// Set master fd to non-blocking mode for async I/O.
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

/// Write text directly to the master fd (message injection).
pub fn inject_text(fd: &OwnedFd, text: &str) -> Result<()> {
    use std::io::Write;
    let mut f = unsafe { std::fs::File::from_raw_fd(fd.as_raw_fd()) };
    let result = f.write_all(text.as_bytes());
    // Don't let File drop close the fd — we don't own it here
    std::mem::forget(f);
    result.context("failed to write to PTY master fd")
}

// ---------------------------------------------------------------------------
// Agent resolution
// ---------------------------------------------------------------------------

/// Resolve an agent name to its binary path.
fn resolve_agent(agent: &str) -> Result<String> {
    let output = std::process::Command::new("which")
        .arg(agent)
        .output()
        .with_context(|| format!("failed to look up \"{agent}\""))?;
    if !output.status.success() {
        bail!("\"{agent}\" not found on PATH. Is it installed?");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Detect a channel name: tmux session if available, otherwise project/hostname.
fn detect_channel() -> String {
    // Try tmux session name
    if let Some(pane) = ipc::detect_tmux_pane() {
        return pane.session;
    }
    // Fall back to git project name or hostname
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

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Launch an agent inside a sidekar-owned PTY.
pub async fn run_agent(agent: &str, args: &[String]) -> Result<()> {
    let bin = resolve_agent(agent)?;
    eprintln!("sidekar pty: launching {agent} ({bin})");

    // Fork the child inside a PTY
    let (master, child_pid) = fork_pty(&bin, args)?;
    let master_raw = master.as_raw_fd();

    // Copy parent terminal size to child PTY
    let _ = copy_terminal_size(master_raw);

    // Set master fd to non-blocking for async I/O
    set_nonblocking(master_raw)?;

    // Build session identity
    let session_id = format!("pty-{child_pid}");
    let channel = detect_channel();
    let nick = crate::bus::pick_nickname_standalone();

    let identity = AgentId {
        name: format!("{agent}-{}-1", channel),
        nick: Some(nick.clone()),
        session: Some(channel.clone()),
        pane: Some(session_id.clone()),
        agent_type: Some("sidekar".into()),
    };

    // Register with broker
    if let Err(e) = broker::register_agent(&identity, Some(&session_id)) {
        eprintln!("sidekar pty: broker registration failed: {e}");
    }

    // Start IPC socket listener
    let master_arc = Arc::new(master);
    let socket_path = match ipc::start_socket_listener(
        &session_id,
        &session_id,
        &channel,
        &identity.name,
        Some(&nick),
        ipc::InputSink::PtyFd(master_arc.clone()),
    ) {
        Ok(path) => {
            if let Err(e) = broker::set_agent_socket_path(&identity.name, Some(path.as_path())) {
                eprintln!("sidekar pty: failed to persist socket path: {e}");
            }
            eprintln!("sidekar pty: registered as \"{}\" aka \"{}\" on channel \"{}\"", identity.name, nick, channel);
            Some(path)
        }
        Err(e) => {
            eprintln!("sidekar pty: IPC socket failed: {e}");
            None
        }
    };

    // Enter raw mode (must happen after eprintln messages)
    let _raw_guard = RawModeGuard::enter()?;

    // Run the async event loop
    let exit_code = event_loop(&master_arc, child_pid).await;

    // Cleanup (raw mode restored automatically by _raw_guard drop)
    drop(_raw_guard);
    if let Err(e) = broker::unregister_agent(&identity.name) {
        eprintln!("sidekar pty: unregister failed: {e}");
    }
    if let Some(ref path) = socket_path {
        let _ = std::fs::remove_file(path);
    }

    match exit_code {
        0 => Ok(()),
        code => {
            // Exit with the child's exit code
            std::process::exit(code);
        }
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
                        // Write to master fd (synchronous — small writes)
                        let data = &buf_in[..n];
                        let written = unsafe {
                            libc::write(master_fd, data.as_ptr() as *const libc::c_void, data.len())
                        };
                        if written < 0 {
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
                            Ok(Err(_)) => break, // child exited or read error
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
