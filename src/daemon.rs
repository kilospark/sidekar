//! Sidekar daemon — single background process owning all long-running subsystems.
//!
//! Subsystems:
//! - ext-bridge: WebSocket listener for Chrome extension
//! - monitor: CDP tab watching (planned)
//! - cron: scheduled actions (planned)
//! - bus-housekeeping: cleanup old messages, orphaned agents

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

fn data_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".sidekar")
}

fn pid_path() -> PathBuf {
    data_dir().join("daemon.pid")
}

fn socket_path() -> PathBuf {
    data_dir().join("daemon.sock")
}

/// Check if daemon is already running.
pub fn is_running() -> bool {
    let pid_file = pid_path();
    if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            unsafe { libc::kill(pid, 0) == 0 }
        } else {
            false
        }
    } else {
        false
    }
}

/// Get the PID of the running daemon, if any.
pub fn get_pid() -> Option<i32> {
    let pid_file = pid_path();
    std::fs::read_to_string(&pid_file)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|&pid| unsafe { libc::kill(pid, 0) == 0 })
}

/// Start the daemon if not already running.
pub fn ensure_running() -> Result<()> {
    if is_running() {
        return Ok(());
    }

    let exe = std::env::current_exe().context("Cannot find sidekar binary")?;
    let child = std::process::Command::new(exe)
        .arg("daemon")
        .arg("run")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .context("Failed to spawn daemon")?;

    eprintln!("Started daemon (PID {})", child.id());

    // Wait for socket to appear
    let sock = socket_path();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    while std::time::Instant::now() < deadline {
        if sock.exists() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!("Daemon did not create socket within 4s (child PID {})", child.id());
}

/// Stop the running daemon.
pub fn stop() -> Result<()> {
    if let Some(pid) = get_pid() {
        unsafe { libc::kill(pid, libc::SIGTERM) };
        eprintln!("Sent SIGTERM to daemon (PID {pid})");
        // Wait for it to exit
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if !is_running() {
                let _ = std::fs::remove_file(pid_path());
                let _ = std::fs::remove_file(socket_path());
                eprintln!("Daemon stopped");
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        bail!("Daemon did not exit within 3s");
    } else {
        eprintln!("Daemon is not running");
        Ok(())
    }
}

/// Show daemon status.
pub fn status() -> Result<()> {
    if let Some(pid) = get_pid() {
        println!("Daemon running (PID {pid})");
        println!("Socket: {}", socket_path().display());
    } else {
        println!("Daemon not running");
    }
    Ok(())
}

/// Send a command to the daemon and return the response.
pub fn send_command(cmd: &Value) -> Result<Value> {
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixStream;

    let sock = socket_path();
    let mut stream = UnixStream::connect(&sock)
        .with_context(|| format!("Cannot connect to daemon at {}", sock.display()))?;

    let mut line = serde_json::to_string(cmd)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()?;

    let mut reader = std::io::BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response)?;

    serde_json::from_str(&response).context("Invalid JSON response from daemon")
}

// ---------------------------------------------------------------------------
// Daemon process (runs when `sidekar daemon run` is invoked)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct DaemonState {
    // ext-bridge state will go here
    ext_port: Option<u16>,
}

/// Run the daemon (called by `sidekar daemon run`).
pub async fn run() -> Result<()> {
    // Ensure data dir exists
    std::fs::create_dir_all(data_dir())?;

    // Clean up stale socket
    let sock_path = socket_path();
    if sock_path.exists() {
        let _ = std::fs::remove_file(&sock_path);
    }

    // Bind unix socket
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("Failed to bind socket at {}", sock_path.display()))?;

    // Set socket permissions (owner only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&sock_path, perms)?;
    }

    // Write PID file
    let pid = std::process::id();
    std::fs::write(pid_path(), pid.to_string())?;

    eprintln!("sidekar daemon running (PID {pid})");
    eprintln!("Socket: {}", sock_path.display());

    let state = std::sync::Arc::new(tokio::sync::Mutex::new(DaemonState::default()));

    // Signal handling for graceful shutdown
    let shutdown_sock = sock_path.clone();
    let shutdown_pid = pid_path();
    tokio::spawn(async move {
        let st = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        let si = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt());
        match (st, si) {
            (Ok(mut sigterm), Ok(mut sigint)) => {
                tokio::select! {
                    _ = sigterm.recv() => {}
                    _ = sigint.recv() => {}
                }
            }
            (Ok(mut sigterm), Err(_)) => { let _ = sigterm.recv().await; }
            (Err(_), Ok(mut sigint)) => { let _ = sigint.recv().await; }
            (Err(_), Err(_)) => { std::future::pending::<()>().await }
        }
        eprintln!("Daemon shutting down...");
        let _ = std::fs::remove_file(&shutdown_pid);
        let _ = std::fs::remove_file(&shutdown_sock);
        std::process::exit(0);
    });

    // TODO: Start ext-bridge subsystem (WebSocket listener for extension)
    // TODO: Start bus-housekeeping subsystem
    // TODO: Start cron subsystem
    // TODO: Start monitor subsystem

    // Accept connections
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let s = state.clone();
                tokio::spawn(handle_connection(stream, s));
            }
            Err(e) => {
                eprintln!("Accept error: {e}");
            }
        }
    }
}

async fn handle_connection(
    stream: UnixStream,
    state: std::sync::Arc<tokio::sync::Mutex<DaemonState>>,
) {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    while let Ok(n) = reader.read_line(&mut line).await {
        if n == 0 {
            break;
        }

        let response = match serde_json::from_str::<Value>(line.trim()) {
            Ok(cmd) => handle_command(&cmd, &state).await,
            Err(e) => json!({"error": format!("Invalid JSON: {e}")}),
        };

        let mut out = serde_json::to_string(&response).unwrap_or_else(|_| r#"{"error":"serialize"}"#.into());
        out.push('\n');
        if writer.write_all(out.as_bytes()).await.is_err() {
            break;
        }
        let _ = writer.flush().await;
        line.clear();
    }
}

async fn handle_command(
    cmd: &Value,
    _state: &std::sync::Arc<tokio::sync::Mutex<DaemonState>>,
) -> Value {
    let cmd_type = cmd.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match cmd_type {
        "ping" => json!({"pong": true}),

        "status" => {
            json!({
                "running": true,
                "pid": std::process::id(),
                // TODO: add ext-bridge status, cron count, monitor count
            })
        }

        "stop" => {
            // Schedule shutdown
            tokio::spawn(async {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                let _ = std::fs::remove_file(pid_path());
                let _ = std::fs::remove_file(socket_path());
                std::process::exit(0);
            });
            json!({"ok": true, "message": "Daemon stopping"})
        }

        // TODO: ext commands (tabs, click, read, etc.)
        // TODO: cron commands (create, list, delete)
        // TODO: monitor commands (start, stop)

        _ => json!({"error": format!("Unknown command: {cmd_type}")}),
    }
}
