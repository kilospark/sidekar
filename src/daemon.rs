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

/// Maximum line length accepted on the daemon socket (1 MB).
/// Prevents memory exhaustion from a malicious local client.
const MAX_LINE_LEN: usize = 1_048_576;

/// Port range for the localhost HTTP/WebSocket listener used by extensions.
const HTTP_PORT_START: u16 = 21517;
const HTTP_PORT_END: u16 = 21527;

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

    // Clean stale pid/socket from a previous crash (kill -9, etc.)
    let _ = std::fs::remove_file(pid_path());
    let _ = std::fs::remove_file(socket_path());

    let exe = std::env::current_exe().context("Cannot find sidekar binary")?;
    let child = std::process::Command::new(exe)
        .arg("daemon")
        .arg("run")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .context("Failed to spawn daemon")?;

    if crate::runtime::verbose() {
        eprintln!("Started daemon (PID {})", child.id());
    }

    // Wait for socket to appear
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
            return run().await;
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

fn restart_current_process() -> Result<()> {
    let pid = std::process::id() as i32;
    spawn_relauncher(pid)?;
    let _ = std::fs::remove_file(pid_path());
    let _ = std::fs::remove_file(socket_path());
    std::process::exit(0);
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

    let state = Arc::new(Mutex::new(DaemonState::new()));

    // Bind localhost HTTP/WS listener for extension communication
    if let Some((tcp_listener, port)) = bind_http_listener() {
        state.lock().await.http_port = port;
        eprintln!("HTTP/WS listener: 127.0.0.1:{port}");
        if let Ok(listener) = tokio::net::TcpListener::from_std(tcp_listener) {
            let tcp_state = state.clone();
            tokio::spawn(accept_http_connections(listener, tcp_state));
        }
    }

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
            (Ok(mut sigterm), Err(_)) => {
                let _ = sigterm.recv().await;
            }
            (Err(_), Ok(mut sigint)) => {
                let _ = sigint.recv().await;
            }
            (Err(_), Err(_)) => std::future::pending::<()>().await,
        }
        eprintln!("Daemon shutting down...");
        let _ = std::fs::remove_file(&shutdown_pid);
        let _ = std::fs::remove_file(&shutdown_sock);
        std::process::exit(0);
    });

    // Start housekeeping subsystem (dead agent sweeper, auto-update, discover heartbeat)
    let http_port = state.lock().await.http_port;
    tokio::spawn(housekeeping_loop(http_port));

    // Start CDP pool idle reaper
    let cdp_pool_for_reaper = state.lock().await.cdp_pool.clone();
    tokio::spawn(cdp_pool_reaper(cdp_pool_for_reaper));

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
        // Upgrade to long-lived CDP proxy connection
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

// ---------------------------------------------------------------------------
// Housekeeping subsystem (dead agent sweeper, auto-update)
// ---------------------------------------------------------------------------

const SWEEP_INTERVAL_SECS: u64 = 60;
const UPDATE_CHECK_INTERVAL_SECS: u64 = 3600; // 1 hour
const STALE_MESSAGE_AGE_SECS: u64 = 3600; // 1 hour

async fn housekeeping_loop(http_port: u16) {
    let mut sweep_interval =
        tokio::time::interval(std::time::Duration::from_secs(SWEEP_INTERVAL_SECS));
    let mut update_interval =
        tokio::time::interval(std::time::Duration::from_secs(UPDATE_CHECK_INTERVAL_SECS));

    // Skip first tick (fires immediately)
    sweep_interval.tick().await;
    update_interval.tick().await;

    loop {
        tokio::select! {
            _ = sweep_interval.tick() => {
                sweep_dead_agents();
                cleanup_stale_messages();
                if http_port > 0 {
                    discover_heartbeat(http_port).await;
                }
            }
            _ = update_interval.tick() => {
                check_for_update().await;
            }
        }
    }
}

/// Periodically reap idle CDP connections from the pool.
async fn cdp_pool_reaper(pool: Arc<Mutex<crate::cdp_proxy::CdpPool>>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
    interval.tick().await; // skip immediate
    loop {
        interval.tick().await;
        pool.lock().await.reap_idle();
    }
}

/// Extract a local process PID from broker pane IDs that encode one.
fn pid_from_agent_pane(pane: &str) -> Option<i32> {
    for prefix in ["pty-", "repl-", "cli-"] {
        if let Some(pid_str) = pane.strip_prefix(prefix) {
            if let Ok(pid) = pid_str.parse::<i32>() {
                return Some(pid);
            }
        }
    }
    None
}

/// Sweep dead agents from the broker. Checks each local agent PID encoded in
/// the pane ID and unregisters agents whose process is no longer alive.
fn sweep_dead_agents() {
    let agents = match crate::broker::list_agents(None) {
        Ok(a) => a,
        Err(_) => return,
    };
    for agent in agents {
        if let Some(ref pane) = agent.id.pane {
            if let Some(pid) = pid_from_agent_pane(pane) {
                if unsafe { libc::kill(pid, 0) } != 0 {
                    let _ = crate::broker::unregister_agent(&agent.id.name);
                }
            }
        }
    }
}

/// Clean up stale messages older than STALE_MESSAGE_AGE_SECS.
fn cleanup_stale_messages() {
    let _ = crate::broker::cleanup_old_messages(STALE_MESSAGE_AGE_SECS);
    let _ = crate::broker::cleanup_old_pending_requests(STALE_MESSAGE_AGE_SECS);
    let _ = crate::broker::cleanup_old_outbound_requests(STALE_MESSAGE_AGE_SECS);
}

/// Check for updates and install in background.
async fn check_for_update() {
    if !crate::config::load_config().auto_update {
        return;
    }
    if !crate::api_client::should_check_for_update() {
        return;
    }
    match crate::api_client::check_for_update().await {
        Ok(Some(latest)) => {
            eprintln!("sidekar: update v{latest} available, installing in background...");
            if let Err(e) = crate::api_client::self_update(&latest).await {
                eprintln!("sidekar: background update failed: {e:#}");
            } else {
                eprintln!("sidekar: updated to v{latest}; restarting daemon...");
                if let Err(e) = restart_current_process() {
                    eprintln!("sidekar: updated, but failed to restart daemon: {e:#}");
                }
            }
        }
        Ok(None) => {}
        Err(_) => {}
    }
}

// ---------------------------------------------------------------------------
// Localhost HTTP/WS listener for extension communication
// ---------------------------------------------------------------------------

fn bind_http_listener() -> Option<(std::net::TcpListener, u16)> {
    for port in HTTP_PORT_START..=HTTP_PORT_END {
        let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
        match std::net::TcpListener::bind(addr) {
            Ok(listener) => {
                listener.set_nonblocking(true).ok();
                return Some((listener, port));
            }
            Err(_) => continue,
        }
    }
    eprintln!("sidekar: could not bind HTTP listener on ports {HTTP_PORT_START}-{HTTP_PORT_END}");
    None
}

async fn accept_http_connections(
    listener: tokio::net::TcpListener,
    state: Arc<Mutex<DaemonState>>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let s = state.clone();
                tokio::spawn(handle_http_connection(stream, s));
            }
            Err(e) => {
                eprintln!("HTTP accept error: {e}");
            }
        }
    }
}

async fn handle_http_connection(mut stream: tokio::net::TcpStream, state: Arc<Mutex<DaemonState>>) {
    let mut buf = [0u8; 4096];
    let n = match stream.peek(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let request = match std::str::from_utf8(&buf[..n]) {
        Ok(s) => s,
        Err(_) => return,
    };

    let first_line = request.lines().next().unwrap_or("");

    if first_line.starts_with("GET /health") {
        let body = r#"{"sidekar":true}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: application/json\r\n\
             x-sidekar: 1\r\n\
             Access-Control-Allow-Origin: *\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes()).await;
        return;
    }

    if first_line.contains("/ext") {
        let ext_state = state.lock().await.ext_state.clone();
        match tokio_tungstenite::accept_async(stream).await {
            Ok(ws) => handle_ext_websocket(ws, ext_state).await,
            Err(e) => {
                if crate::runtime::verbose() {
                    eprintln!("WS handshake failed: {e}");
                }
            }
        }
        return;
    }

    let response = "HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n";
    let _ = stream.write_all(response.as_bytes()).await;
}

async fn handle_ext_websocket(
    ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    ext_state: SharedExtState,
) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::protocol::Message;

    let (mut ws_tx, mut ws_rx) = ws.split();

    let welcome = json!({"type": "welcome", "version": env!("CARGO_PKG_VERSION")});
    if ws_tx
        .send(Message::Text(welcome.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // Wait for bridge_register
    let (ext_token, agent_id) = loop {
        match ws_rx.next().await {
            Some(Ok(Message::Text(text))) => {
                if let Ok(val) = serde_json::from_str::<Value>(&text) {
                    if val.get("type").and_then(|v| v.as_str()) == Some("bridge_register") {
                        let token = val
                            .get("token")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let aid = val
                            .get("agent_id")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        break (token, aid);
                    }
                }
            }
            _ => return,
        }
    };

    // Verify ext token
    let cli_logged_in = crate::auth::auth_token().is_some();
    let user_id = if ext_token.is_empty() {
        None
    } else {
        match tokio::task::spawn_blocking({
            let token = ext_token.clone();
            move || crate::ext::verify_ext_token(&token)
        })
        .await
        {
            Ok(Ok(uid)) => Some(uid),
            _ => None,
        }
    };

    if user_id.is_none() {
        let reason = if ext_token.is_empty() {
            "No extension token — sign in from the extension popup."
        } else if !cli_logged_in {
            "CLI not logged in. Run: sidekar login"
        } else {
            "Extension token verification failed — try signing in again."
        };
        let fail = json!({"type": "auth_fail", "reason": reason, "cli_logged_in": cli_logged_in});
        let _ = ws_tx.send(Message::Text(fail.to_string().into())).await;
        return;
    }

    let user_id = user_id.unwrap();

    let (conn_id, mut bridge_rx, profile) =
        crate::ext::register_bridge_ws(&ext_state, user_id.clone(), agent_id).await;

    let ok = json!({"type": "auth_ok", "cli_logged_in": cli_logged_in, "profile": profile});
    if ws_tx
        .send(Message::Text(ok.to_string().into()))
        .await
        .is_err()
    {
        crate::ext::disconnect_bridge_by_id(&ext_state, conn_id).await;
        return;
    }

    eprintln!(
        "[sidekar] Extension bridge connected via WebSocket (conn: {conn_id}, user: {user_id})"
    );

    // Keepalive task — send pings via bridge_tx, check last_contact for timeout
    let ka_state = ext_state.clone();
    let ka_conn_id = conn_id;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
        interval.tick().await; // skip immediate
        loop {
            interval.tick().await;
            let now = crate::message::epoch_secs();
            let should_disconnect;
            {
                let s = ka_state.lock().await;
                match s.connections.get(&ka_conn_id) {
                    Some(conn) => {
                        should_disconnect = now - conn.last_contact > 30;
                        if !should_disconnect {
                            let ping =
                                serde_json::to_string(&json!({"type":"ping"})).unwrap_or_default();
                            let _ = conn.bridge_tx.send(ping);
                        }
                    }
                    None => break,
                }
            }
            if should_disconnect {
                eprintln!("[sidekar] Extension WS keepalive timeout (conn {ka_conn_id})");
                crate::ext::disconnect_bridge_by_id(&ka_state, ka_conn_id).await;
                break;
            }
        }
    });

    loop {
        tokio::select! {
            Some(outbound) = bridge_rx.recv() => {
                if ws_tx.send(Message::Text(outbound.into())).await.is_err() {
                    break;
                }
            }
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(val) = serde_json::from_str::<Value>(&text) {
                            crate::ext::touch_connection(&ext_state, conn_id).await;
                            let msg_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            if msg_type == "pong" {
                                continue;
                            }
                            if msg_type == "watch_event" {
                                let wid = val.get("watchId").and_then(|v| v.as_str()).unwrap_or("");
                                let current = val.get("current").and_then(|v| v.as_str()).unwrap_or("");
                                let previous = val.get("previous").and_then(|v| v.as_str()).unwrap_or("");
                                let url = val.get("url").and_then(|v| v.as_str());
                                if !wid.is_empty() {
                                    if let Err(e) = crate::ext::deliver_watch_event(
                                        &ext_state, wid, current, previous, url,
                                    )
                                    .await
                                    {
                                        eprintln!("[sidekar] watch event delivery failed: {e}");
                                    }
                                }
                                continue;
                            }
                            crate::ext::resolve_pending(&ext_state, conn_id, val).await;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }

    crate::ext::disconnect_bridge_by_id(&ext_state, conn_id).await;
    eprintln!("[sidekar] Extension WS bridge disconnected (conn: {conn_id})");
}

async fn discover_heartbeat(port: u16) {
    if crate::auth::auth_token().is_none() {
        return;
    }
    if let Err(e) = crate::api_client::register_discover_port(port).await {
        if crate::runtime::verbose() {
            eprintln!("sidekar: discover heartbeat failed: {e:#}");
        }
    }
}

async fn handle_command(cmd: &Value, state: &Arc<Mutex<DaemonState>>) -> Value {
    let cmd_type = cmd.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match cmd_type {
        "ping" => json!({"pong": true}),

        "status" => {
            let s = state.lock().await;
            let ext_status = crate::ext::get_status(&s.ext_state).await;
            let cli_logged_in = crate::auth::auth_token().is_some();
            json!({
                "running": true,
                "pid": std::process::id(),
                "http_port": s.http_port,
                "ext": ext_status,
                "cli_logged_in": cli_logged_in,
            })
        }

        "stop" => {
            tokio::spawn(async {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                let _ = std::fs::remove_file(pid_path());
                let _ = std::fs::remove_file(socket_path());
                std::process::exit(0);
            });
            json!({"ok": true, "message": "Daemon stopping"})
        }

        // Extension commands - forward to ext-bridge
        "ext" => {
            let ext_cmd = cmd.get("command").cloned().unwrap_or(json!({}));
            let agent_id = cmd
                .get("agent_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            let target_conn = cmd.get("conn_id").and_then(|v| v.as_u64());
            let target_profile = cmd
                .get("profile")
                .and_then(|v| v.as_str())
                .map(String::from);
            let deliver_to = cmd
                .get("deliver_to")
                .and_then(|v| v.as_str())
                .map(String::from);

            // Capture the inner command name before forwarding for bookkeeping.
            let inner_cmd = ext_cmd
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let inner_selector = ext_cmd
                .get("selector")
                .and_then(|v| v.as_str())
                .map(String::from);
            let inner_watch_id = ext_cmd
                .get("watchId")
                .and_then(|v| v.as_str())
                .map(String::from);

            let ext_state = {
                let s = state.lock().await;
                s.ext_state.clone()
            };
            let result = crate::ext::forward_command(
                &ext_state,
                ext_cmd,
                agent_id,
                target_conn,
                target_profile,
            )
            .await;

            // Post-process: maintain watch registry.
            if inner_cmd == "watch" && result.get("error").is_none() {
                if let (Some(wid), Some(sel), Some(dest)) = (
                    result
                        .get("watchId")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    result
                        .get("selector")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .or(inner_selector),
                    deliver_to,
                ) {
                    // Find which connection ended up servicing this watch.
                    let (conn_id, profile) = {
                        let s = ext_state.lock().await;
                        if let Some(cid) = target_conn {
                            let profile = s
                                .connections
                                .get(&cid)
                                .map(|c| c.profile.clone())
                                .unwrap_or_default();
                            (cid, profile)
                        } else {
                            s.connections
                                .iter()
                                .next()
                                .map(|(k, c)| (*k, c.profile.clone()))
                                .unwrap_or((0, String::new()))
                        }
                    };
                    crate::ext::register_watch(&ext_state, wid, sel, dest, conn_id, profile).await;
                }
            } else if inner_cmd == "unwatch" && result.get("error").is_none() {
                if let Some(wid) = inner_watch_id {
                    crate::ext::remove_watch(&ext_state, &wid).await;
                } else {
                    // Bulk unwatch — the extension removed everything; clear our map.
                    let mut s = ext_state.lock().await;
                    s.watches.clear();
                }
            }

            // Annotate the response with deliver_to so the CLI can display it.
            let mut final_result = result;
            if inner_cmd == "watch" && final_result.is_object() {
                if let Some(dest) = final_result
                    .get("watchId")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                {
                    let _ = dest; // just checking existence
                    if let Some(obj) = final_result.as_object_mut() {
                        // Look up deliver_to from the just-registered watch (if any).
                        let deliver = {
                            let s = ext_state.lock().await;
                            obj.get("watchId")
                                .and_then(|v| v.as_str())
                                .and_then(|wid| s.watches.get(wid).map(|w| w.deliver_to.clone()))
                        };
                        if let Some(d) = deliver {
                            obj.insert("deliverTo".into(), json!(d));
                        }
                    }
                }
            }

            final_result
        }

        "ext_status" => {
            let s = state.lock().await;
            crate::ext::get_status(&s.ext_state).await
        }

        _ => json!({"error": format!("Unknown command: {cmd_type}")}),
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
            // EOF
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
            // Drain remaining bytes until newline or EOF to avoid desync
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
mod tests {
    use super::pid_from_agent_pane;

    #[test]
    fn pid_from_agent_pane_recognizes_local_agent_prefixes() {
        assert_eq!(pid_from_agent_pane("pty-123"), Some(123));
        assert_eq!(pid_from_agent_pane("repl-456"), Some(456));
        assert_eq!(pid_from_agent_pane("cli-789"), Some(789));
    }

    #[test]
    fn pid_from_agent_pane_rejects_non_pid_panes() {
        assert_eq!(pid_from_agent_pane("tab-123"), None);
        assert_eq!(pid_from_agent_pane("repl-abc"), None);
        assert_eq!(pid_from_agent_pane("pty-"), None);
        assert_eq!(pid_from_agent_pane(""), None);
    }
}
