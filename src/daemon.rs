//! Sidekar daemon — single background process owning all long-running subsystems.
//!
//! Subsystems:
//! - ext-bridge: extension bridge state and routing
//! - monitor: CDP tab watching (planned)
//! - cron: scheduled actions (planned)
//! - bus-housekeeping: cleanup old messages, orphaned agents

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use crate::ext::{ExtState, SharedExtState};

mod command;
mod housekeeping;
mod http;

use command::handle_command;
use housekeeping::{cdp_pool_reaper, housekeeping_loop};
use http::{accept_http_connections, bind_http_listener};

/// Maximum line length accepted on the daemon socket (1 MB).
/// Prevents memory exhaustion from a malicious local client.
const MAX_LINE_LEN: usize = 1_048_576;

fn data_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".sidekar")
}

fn pid_path() -> PathBuf {
    data_dir().join("daemon.pid")
}

pub fn socket_path() -> PathBuf {
    data_dir().join("daemon.sock")
}

fn pid_file_pid() -> Option<i32> {
    std::fs::read_to_string(pid_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

fn pid_is_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn daemon_ping_pid() -> Option<Option<i32>> {
    send_command(&json!({"type": "ping"}))
        .ok()
        .filter(|v| v.get("pong").and_then(Value::as_bool).unwrap_or(false))
        .map(|v| v.get("pid").and_then(Value::as_i64).map(|pid| pid as i32))
}

fn running_daemon_pid() -> Option<i32> {
    if !socket_path().exists() {
        return None;
    }
    let pid = match daemon_ping_pid()? {
        Some(pid) => pid,
        None => pid_file_pid()?,
    };
    if !pid_is_alive(pid) {
        return None;
    }
    if pid_file_pid() != Some(pid) {
        let _ = std::fs::write(pid_path(), pid.to_string());
    }
    Some(pid)
}

/// Check if daemon is already running.
pub fn is_running() -> bool {
    running_daemon_pid().is_some()
}

/// Get the PID of the running daemon, if any.
pub fn get_pid() -> Option<i32> {
    running_daemon_pid()
}

/// Start the daemon if not already running.
pub fn ensure_running() -> Result<()> {
    if is_running() {
        return Ok(());
    }

    let _ = std::fs::remove_file(pid_path());
    let _ = std::fs::remove_file(socket_path());

    let exe = std::env::current_exe().context("Cannot find sidekar binary")?;
    let child = std::process::Command::new(exe)
        .arg("daemon")
        .arg("start")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .context("Failed to spawn daemon")?;

    if crate::runtime::verbose() {
        eprintln!("Started daemon (PID {})", child.id());
    }

    let sock = socket_path();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    while std::time::Instant::now() < deadline {
        if sock.exists() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!(
        "Daemon did not create socket within 4s (child PID {})",
        child.id()
    );
}

/// Restart the daemon if it is currently running.
/// Returns true if a restart was performed.
pub fn restart_if_running() -> Result<bool> {
    if !is_running() {
        return Ok(false);
    }
    stop()?;
    ensure_running()?;
    Ok(true)
}

/// Restart the daemon unconditionally.
/// If it is not running, this starts it.
pub fn restart() -> Result<()> {
    if is_running() {
        stop()?;
    }
    ensure_running()?;
    Ok(())
}

fn process_exists(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Helper entrypoint used during daemon self-update.
/// Waits for the old daemon PID to exit, then starts the new daemon.
pub async fn relaunch_after_exit(old_pid: i32) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if !process_exists(old_pid) {
            return start().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    bail!("Timed out waiting for daemon PID {old_pid} to exit before relaunch");
}

fn spawn_relauncher(old_pid: i32) -> Result<()> {
    let exe = std::env::current_exe().context("Cannot find sidekar binary")?;
    std::process::Command::new(exe)
        .arg("daemon")
        .arg("relaunch")
        .arg(old_pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .context("Failed to spawn daemon relaunch helper")?;
    Ok(())
}

pub(super) fn restart_current_process() -> Result<()> {
    let pid = std::process::id() as i32;
    spawn_relauncher(pid)?;
    let _ = std::fs::remove_file(pid_path());
    let _ = std::fs::remove_file(socket_path());
    std::process::exit(0);
}

/// Stop the running daemon.
pub fn stop() -> Result<()> {
    if let Some(pid) = get_pid() {
        match send_command(&json!({"type": "stop"})) {
            Ok(_) => eprintln!("Sent stop command to daemon (PID {pid})"),
            Err(e) => {
                eprintln!("Daemon stop command failed ({e:#}); sending SIGTERM to PID {pid}");
                unsafe { libc::kill(pid, libc::SIGTERM) };
            }
        }
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
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;

    let mut line = serde_json::to_string(cmd)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()?;

    let mut reader = std::io::BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response)?;

    serde_json::from_str(&response).context("Invalid JSON response from daemon")
}

struct DaemonState {
    ext_state: SharedExtState,
    cdp_pool: Arc<Mutex<crate::cdp_proxy::CdpPool>>,
    http_port: u16,
}

impl DaemonState {
    fn new() -> Self {
        Self {
            ext_state: Arc::new(Mutex::new(ExtState::default())),
            cdp_pool: Arc::new(Mutex::new(crate::cdp_proxy::CdpPool::new())),
            http_port: 0,
        }
    }
}

/// Run the daemon (called by `sidekar daemon start`).
pub async fn start() -> Result<()> {
    std::fs::create_dir_all(data_dir())?;

    housekeeping::kill_orphaned_daemons();

    let sock_path = socket_path();
    if sock_path.exists() {
        let _ = std::fs::remove_file(&sock_path);
    }

    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("Failed to bind socket at {}", sock_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&sock_path, perms)?;
    }

    let pid = std::process::id();
    std::fs::write(pid_path(), pid.to_string())?;

    eprintln!("sidekar daemon running (PID {pid})");
    eprintln!("Socket: {}", sock_path.display());

    let state = Arc::new(Mutex::new(DaemonState::new()));

    if let Some((tcp_listener, port)) = bind_http_listener() {
        state.lock().await.http_port = port;
        eprintln!("HTTP/WS listener: 127.0.0.1:{port}");
        if let Ok(listener) = tokio::net::TcpListener::from_std(tcp_listener) {
            let tcp_state = state.clone();
            tokio::spawn(accept_http_connections(listener, tcp_state));
        }
    }

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
            (Ok(mut sigterm), Err(_)) => {
                let _ = sigterm.recv().await;
            }
            (Err(_), Ok(mut sigint)) => {
                let _ = sigint.recv().await;
            }
            (Err(_), Err(_)) => std::future::pending::<()>().await,
        }
        eprintln!("Daemon shutting down...");
        crate::api_client::deregister_discover_port().await;
        let _ = std::fs::remove_file(&shutdown_pid);
        let _ = std::fs::remove_file(&shutdown_sock);
        std::process::exit(0);
    });

    let http_port = state.lock().await.http_port;
    tokio::spawn(housekeeping_loop(http_port));

    let cdp_pool_for_reaper = state.lock().await.cdp_pool.clone();
    tokio::spawn(cdp_pool_reaper(cdp_pool_for_reaper));

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

    match read_line_limited(&mut reader, &mut line, MAX_LINE_LEN).await {
        Ok(0) | Err(_) => return,
        _ => {}
    }

    let first = match serde_json::from_str::<Value>(line.trim()) {
        Ok(cmd) => cmd,
        Err(e) => {
            let response = json!({"error": format!("Invalid JSON: {e}")});
            let mut out = serde_json::to_string(&response)
                .unwrap_or_else(|_| r#"{"error":"serialize"}"#.into());
            out.push('\n');
            let _ = writer.write_all(out.as_bytes()).await;
            let _ = writer.flush().await;
            return;
        }
    };
    line.clear();

    if first.get("type").and_then(|v| v.as_str()) == Some("cdp_connect") {
        let ws_url = first
            .get("ws_url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if ws_url.is_empty() {
            let err = serde_json::to_string(&json!({"type":"cdp_resp","error":"missing ws_url"}))
                .unwrap_or_default();
            let _ = writer.write_all(err.as_bytes()).await;
            let _ = writer.write_all(b"\n").await;
            let _ = writer.flush().await;
            return;
        }
        let pool = state.lock().await.cdp_pool.clone();
        crate::cdp_proxy::handle_cdp_connection(ws_url, reader, writer, pool).await;
        return;
    }

    let mut current = Some(first);
    loop {
        let cmd = match current.take() {
            Some(v) => v,
            None => match read_line_limited(&mut reader, &mut line, MAX_LINE_LEN).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let parsed = serde_json::from_str::<Value>(line.trim());
                    line.clear();
                    match parsed {
                        Ok(v) => v,
                        Err(e) => {
                            let response = json!({"error": format!("Invalid JSON: {e}")});
                            let mut out = serde_json::to_string(&response)
                                .unwrap_or_else(|_| r#"{"error":"serialize"}"#.into());
                            out.push('\n');
                            if writer.write_all(out.as_bytes()).await.is_err() {
                                break;
                            }
                            let _ = writer.flush().await;
                            continue;
                        }
                    }
                }
            },
        };

        let response = handle_command(&cmd, &state).await;
        let mut out =
            serde_json::to_string(&response).unwrap_or_else(|_| r#"{"error":"serialize"}"#.into());
        out.push('\n');
        if writer.write_all(out.as_bytes()).await.is_err() {
            break;
        }
        let _ = writer.flush().await;
    }
}

/// Read a line from the socket, rejecting lines longer than `max_len` bytes.
/// Uses `read_until` on a pre-capped buffer to avoid unbounded allocation.
async fn read_line_limited(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    buf: &mut String,
    max_len: usize,
) -> std::io::Result<usize> {
    use tokio::io::AsyncReadExt;
    buf.clear();
    let mut raw = Vec::with_capacity(4096);
    loop {
        let mut byte = [0u8; 1];
        let n = reader.read(&mut byte).await?;
        if n == 0 {
            if raw.is_empty() {
                return Ok(0);
            }
            break;
        }
        if byte[0] == b'\n' {
            raw.push(byte[0]);
            break;
        }
        raw.push(byte[0]);
        if raw.len() > max_len {
            loop {
                let n = reader.read(&mut byte).await?;
                if n == 0 || byte[0] == b'\n' {
                    break;
                }
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "line exceeds maximum length",
            ));
        }
    }
    let s = String::from_utf8(raw)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let len = s.len();
    *buf = s;
    Ok(len)
}

#[cfg(test)]
mod tests;
