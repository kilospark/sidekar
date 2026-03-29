//! Extension bridge — WebSocket server for Chrome extension communication.
//!
//! `sidekar ext-server` runs a WS server on 127.0.0.1:9876.
//! `sidekar ext <command>` auto-launches the server if needed, then sends a command.

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, oneshot};

use crate::auth;

const DEFAULT_API_URL: &str = "https://sidekar.dev";

const DEFAULT_PORT: u16 = 9876;
const TIMEOUT_SECS: u64 = 30;

fn ipc_port_for_ws(port: u16) -> Result<u16> {
    port.checked_add(1)
        .ok_or_else(|| anyhow!("SIDEKAR_EXT_PORT cannot be 65535 (IPC needs port+1)"))
}

fn data_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".sidekar")
}

fn pid_path() -> PathBuf {
    data_dir().join("ext-server.pid")
}

// ---------------------------------------------------------------------------
// Shared state between server and extension connection
// ---------------------------------------------------------------------------

pub struct ExtState {
    pub ext_tx: Option<
        futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<TcpStream>,
            tokio_tungstenite::tungstenite::Message,
        >,
    >,
    pub pending: HashMap<String, oneshot::Sender<Value>>,
    pub connected: bool,
    pub authenticated: bool,
    pub verified_user_id: Option<String>,
}

impl Default for ExtState {
    fn default() -> Self {
        Self {
            ext_tx: None,
            pending: HashMap::new(),
            connected: false,
            authenticated: false,
            verified_user_id: None,
        }
    }
}

pub type SharedExtState = Arc<Mutex<ExtState>>;

fn ext_api_base() -> String {
    std::env::var("SIDEKAR_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string())
}

/// Verify that the extension token and CLI device token belong to the same user.
/// Calls the sidekar.dev API and returns the user_id on success.
async fn verify_ext_token(ext_token: &str) -> Result<String> {
    let device_token = auth::auth_token().ok_or_else(|| anyhow!("Run `sidekar login`"))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let url = format!("{}/api/auth/ext-token?verify=1", ext_api_base());
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", device_token))
        .json(&json!({ "ext_token": ext_token }))
        .send()
        .await
        .context("Failed to contact sidekar.dev for token verification")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("Token verification failed: HTTP {status} — {body}");
    }

    let data: Value = resp
        .json()
        .await
        .context("Invalid response from verify-ext")?;

    let matched = data.get("match").and_then(|v| v.as_bool()).unwrap_or(false);
    if !matched {
        bail!("Extension token and CLI token belong to different users");
    }

    data.get("user_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("No user_id in verification response"))
}

type SharedState = Arc<Mutex<ExtState>>;

// ---------------------------------------------------------------------------
// Ext bridge for daemon
// ---------------------------------------------------------------------------

/// Start the extension bridge WebSocket listener.
/// Called by the daemon to run ext-bridge as a subsystem.
/// Returns the port number on success.
pub async fn start_ext_bridge(state: SharedExtState) -> Result<u16> {
    let port = std::env::var("SIDEKAR_EXT_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .with_context(|| {
            format!("Failed to bind ext-bridge on port {port}. Is another instance running?")
        })?;

    eprintln!("ext-bridge listening on ws://127.0.0.1:{port}");

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    eprintln!("Extension connection from {addr}");
                    let s = state.clone();
                    tokio::spawn(handle_extension_connection(stream, s));
                }
                Err(e) => {
                    eprintln!("ext-bridge accept error: {e}");
                }
            }
        }
    });

    Ok(port)
}

/// Send a command to the extension via the shared state.
/// Used by daemon to forward ext commands from unix socket.
pub async fn forward_command(state: &SharedExtState, command: Value) -> Value {
    match send_command(state, command).await {
        Ok(v) => v,
        Err(e) => json!({"error": e.to_string()}),
    }
}

/// Get extension connection status.
pub async fn get_status(state: &SharedExtState) -> Value {
    let s = state.lock().await;
    json!({
        "connected": s.connected,
        "authenticated": s.authenticated,
    })
}

// ---------------------------------------------------------------------------
// Standalone server (legacy - will be deprecated)
// ---------------------------------------------------------------------------

pub async fn run_server() -> Result<()> {
    let port = std::env::var("SIDEKAR_EXT_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .with_context(|| {
            format!(
                "Failed to bind to port {port}. Another ext-server is probably already running. Try: sidekar ext stop"
            )
        })?;

    let ipc_port = ipc_port_for_ws(port)?;
    let ipc_listener = TcpListener::bind(format!("127.0.0.1:{ipc_port}"))
        .await
        .with_context(|| {
            format!(
                "Failed to bind IPC port {ipc_port} (WebSocket uses {port}). Try: sidekar ext stop"
            )
        })?;

    let pid = std::process::id();
    std::fs::create_dir_all(data_dir())?;
    std::fs::write(pid_path(), pid.to_string())?;

    eprintln!("sidekar ext-server listening on ws://127.0.0.1:{port}");
    eprintln!("PID: {pid}");

    let state: SharedState = Arc::new(Mutex::new(ExtState {
        ext_tx: None,
        pending: HashMap::new(),
        connected: false,
        authenticated: false,
        verified_user_id: None,
    }));

    let ipc_state = state.clone();
    tokio::spawn(async move {
        loop {
            if let Ok((stream, _)) = ipc_listener.accept().await {
                let s = ipc_state.clone();
                tokio::spawn(handle_cli_connection(stream, s));
            }
        }
    });

    // Handle SIGTERM/SIGINT for clean shutdown (registration can fail under FD pressure — do not panic).
    let shutdown_pid_path = pid_path();
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
            (Err(e1), Err(e2)) => {
                eprintln!(
                    "sidekar: signal handlers unavailable ({e1}; {e2}); use kill to stop ext-server"
                );
                std::future::pending::<()>().await
            }
        }
        let _ = std::fs::remove_file(&shutdown_pid_path);
        std::process::exit(0);
    });

    loop {
        let (stream, addr) = listener.accept().await?;
        eprintln!("Connection from {addr}");
        let s = state.clone();
        tokio::spawn(handle_extension_connection(stream, s));
    }
}

async fn handle_extension_connection(stream: TcpStream, state: SharedState) {
    let ws = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("WebSocket handshake failed: {e}");
            return;
        }
    };

    let (tx, mut rx) = ws.split();

    {
        let mut s = state.lock().await;
        if s.ext_tx.is_some() {
            // Close old stale connection and accept the new one
            eprintln!("Replacing stale extension connection with new one");
            if let Some(mut old_tx) = s.ext_tx.take() {
                let _ = old_tx.close().await;
            }
            s.pending.clear();
        }
        s.ext_tx = Some(tx);
        s.connected = true;
        s.authenticated = false;
        s.verified_user_id = None;
    }

    eprintln!("WebSocket established, waiting for auth...");

    while let Some(msg) = rx.next().await {
        match msg {
            Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                if let Ok(val) = serde_json::from_str::<Value>(&text) {
                    let msg_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");

                    if msg_type == "ping" {
                        // Respond to keepalive pings
                        let mut s = state.lock().await;
                        if let Some(ref mut ext_tx) = s.ext_tx {
                            let _ = ext_tx
                                .send(tokio_tungstenite::tungstenite::Message::Text(
                                    json!({"type": "pong"}).to_string().into(),
                                ))
                                .await;
                        }
                        continue;
                    }

                    if msg_type == "hello" {
                        let version = val.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                        let provided_token =
                            val.get("token").and_then(|v| v.as_str()).unwrap_or("");

                        if !provided_token.is_empty() {
                            // Token-based auth (OAuth flow)
                            match verify_ext_token(provided_token).await {
                                Ok(user_id) => {
                                    let mut s = state.lock().await;
                                    s.authenticated = true;
                                    s.verified_user_id = Some(user_id.clone());
                                    eprintln!(
                                        "Extension authenticated via token: v{version} (user {user_id})"
                                    );
                                    if let Some(ref mut ext_tx) = s.ext_tx {
                                        let _ = ext_tx
                                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                                json!({"type": "auth_ok"}).to_string().into(),
                                            ))
                                            .await;
                                    }
                                }
                                Err(e) => {
                                    let reason = format!("{e:#}");
                                    eprintln!("Extension token auth failed: {reason}");
                                    let mut s = state.lock().await;
                                    if let Some(ref mut ext_tx) = s.ext_tx {
                                        // Send auth_fail message
                                        let _ = ext_tx
                                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                                json!({"type": "auth_fail", "reason": reason})
                                                    .to_string()
                                                    .into(),
                                            ))
                                            .await;
                                        // Send close frame after the message (ensures ordering)
                                        let _ = ext_tx
                                            .send(tokio_tungstenite::tungstenite::Message::Close(
                                                None,
                                            ))
                                            .await;
                                        let _ = ext_tx.flush().await;
                                    }
                                    break;
                                }
                            }
                        } else {
                            let reason = "No token provided — log in from the extension popup";
                            eprintln!("Extension auth failed: {reason}");
                            let mut s = state.lock().await;
                            if let Some(ref mut ext_tx) = s.ext_tx {
                                let _ = ext_tx
                                    .send(tokio_tungstenite::tungstenite::Message::Text(
                                        json!({"type": "auth_fail", "reason": reason})
                                            .to_string()
                                            .into(),
                                    ))
                                    .await;
                                let _ = ext_tx
                                    .send(tokio_tungstenite::tungstenite::Message::Close(None))
                                    .await;
                                let _ = ext_tx.flush().await;
                            }
                            break;
                        }
                        continue;
                    }

                    // All other messages require authentication
                    {
                        let s = state.lock().await;
                        if !s.authenticated {
                            eprintln!("Ignoring unauthenticated message");
                            continue;
                        }
                    }

                    // Route response to pending request
                    if let Some(id) = val.get("id").and_then(|v| v.as_str()) {
                        let mut s = state.lock().await;
                        if let Some(tx) = s.pending.remove(id) {
                            let _ = tx.send(val);
                        }
                    }
                }
            }
            Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => break,
            Err(e) => {
                eprintln!("WebSocket error: {e}");
                break;
            }
            _ => {}
        }
    }

    let pending = {
        let mut s = state.lock().await;
        let pending = std::mem::take(&mut s.pending);
        s.ext_tx = None;
        s.connected = false;
        s.authenticated = false;
        s.verified_user_id = None;
        pending
    };
    for (_id, tx) in pending {
        let _ = tx.send(json!({"error": "Extension disconnected"}));
    }
    eprintln!("Extension disconnected");
}

async fn send_command(state: &SharedState, command: Value) -> Result<Value> {
    let id = format!("{:08x}", rand::random::<u32>());
    let mut msg = command;
    msg.as_object_mut().unwrap().insert("id".into(), json!(id));

    let (tx, rx) = oneshot::channel();

    {
        let mut s = state.lock().await;
        if !s.connected || !s.authenticated || s.ext_tx.is_none() {
            bail!("Extension not connected. Is Chrome running with the Sidekar extension?");
        }
        s.pending.insert(id.clone(), tx);
        let text = serde_json::to_string(&msg)?;
        if let Some(ref mut ext_tx) = s.ext_tx {
            match ext_tx
                .send(tokio_tungstenite::tungstenite::Message::Text(text.into()))
                .await
            {
                Ok(()) => {}
                Err(e) => {
                    s.pending.remove(&id);
                    return Err(anyhow::Error::from(e)).context("Failed to send to extension");
                }
            }
        }
    }

    match tokio::time::timeout(std::time::Duration::from_secs(TIMEOUT_SECS), rx).await {
        Ok(Ok(val)) => Ok(val),
        Ok(Err(_)) => bail!("Extension response channel closed"),
        Err(_) => {
            state.lock().await.pending.remove(&id);
            bail!("Extension command timed out ({TIMEOUT_SECS}s)")
        }
    }
}

async fn handle_cli_connection(stream: TcpStream, state: SharedState) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const MAX_IPC_MSG: usize = 65536;
    let mut stream = stream;
    let mut buf = Vec::with_capacity(4096);

    // Read until EOF (CLI shuts down its write half after sending)
    loop {
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                if buf.len() + n > MAX_IPC_MSG {
                    let err = json!({"error": "IPC message too large"});
                    let _ = stream
                        .write_all(serde_json::to_string(&err).unwrap().as_bytes())
                        .await;
                    return;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Err(_) => return,
        }
    }
    if buf.is_empty() {
        return;
    }

    let command: Value = match serde_json::from_slice(&buf) {
        Ok(v) => v,
        Err(e) => {
            let err = json!({"error": format!("Invalid JSON: {e}")});
            let _ = stream
                .write_all(serde_json::to_string(&err).unwrap().as_bytes())
                .await;
            return;
        }
    };

    // Quick status query — answered by server, not forwarded to extension
    if command.get("query").and_then(|v| v.as_str()) == Some("status") {
        let s = state.lock().await;
        let result = json!({
            "connected": s.connected,
            "authenticated": s.authenticated,
        });
        let _ = stream
            .write_all(serde_json::to_string(&result).unwrap().as_bytes())
            .await;
        return;
    }

    let result = match send_command(&state, command).await {
        Ok(v) => v,
        Err(e) => json!({"error": e.to_string()}),
    };

    let _ = stream
        .write_all(serde_json::to_string(&result).unwrap().as_bytes())
        .await;
}

// ---------------------------------------------------------------------------
// Auto-launch: ensure ext-server is running
// ---------------------------------------------------------------------------

pub fn is_server_running() -> bool {
    let pid_file = pid_path();
    if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            // Check if process is alive
            unsafe { libc::kill(pid, 0) == 0 }
        } else {
            false
        }
    } else {
        false
    }
}

/// Check if the extension is connected and authenticated (blocking, 500ms max).
///
/// Used by the auto-routing logic in main.rs to decide whether browser commands
/// should be routed through the Chrome extension instead of CDP.
pub fn is_ext_available() -> bool {
    use std::io::{Read, Write};

    if !is_server_running() {
        return false;
    }

    let port = std::env::var("SIDEKAR_EXT_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let ipc_port = match ipc_port_for_ws(port) {
        Ok(p) => p,
        Err(_) => return false,
    };

    let timeout = std::time::Duration::from_millis(500);

    let mut stream = match std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{ipc_port}").parse().unwrap(),
        timeout,
    ) {
        Ok(s) => s,
        Err(_) => return false,
    };

    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));

    let msg = r#"{"query":"status"}"#;
    if stream.write_all(msg.as_bytes()).is_err() {
        return false;
    }
    // Shut down the write half so the server sees EOF and responds
    if stream.shutdown(std::net::Shutdown::Write).is_err() {
        return false;
    }

    let mut buf = Vec::with_capacity(256);
    if stream.read_to_end(&mut buf).is_err() {
        return false;
    }

    match serde_json::from_slice::<Value>(&buf) {
        Ok(val) => val
            .get("authenticated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        Err(_) => false,
    }
}

pub fn auto_launch_server() -> Result<()> {
    if is_server_running() {
        return Ok(());
    }

    // Find our own binary
    let exe = std::env::current_exe().context("Cannot find sidekar binary")?;

    // Spawn detached ext-server process (logs to stderr, errors to SQLite)
    let child = std::process::Command::new(exe)
        .arg("ext-server")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .context("Failed to spawn ext-server")?;

    if std::env::var("SIDEKAR_VERBOSE").is_ok() {
        eprintln!("Started ext-server (PID {})", child.id());
    }

    let port = std::env::var("SIDEKAR_EXT_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let ipc_port = ipc_port_for_ws(port)?;
    let ipc_addr = format!("127.0.0.1:{ipc_port}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    while std::time::Instant::now() < deadline {
        if std::net::TcpStream::connect(&ipc_addr).is_ok() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!(
        "ext-server did not open IPC on {ipc_addr} within 4s (child PID {})",
        child.id()
    );
}

// ---------------------------------------------------------------------------
// CLI client
// ---------------------------------------------------------------------------

pub async fn send_cli_command(
    command: &str,
    args: &[String],
    default_tab: Option<u64>,
) -> Result<()> {
    // Handle meta commands
    if command == "stop" {
        return stop_server();
    }
    if command == "status" {
        return show_status();
    }

    // Auto-launch server
    auto_launch_server()?;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let port = std::env::var("SIDEKAR_EXT_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let ipc_port = ipc_port_for_ws(port)?;

    let msg = build_command(command, args, default_tab)?;

    // Retry connection a few times (server may still be starting)
    let mut stream = None;
    for attempt in 0..5 {
        match TcpStream::connect(format!("127.0.0.1:{ipc_port}")).await {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(_) if attempt < 4 => {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
            Err(e) => bail!("Cannot connect to ext-server: {e}"),
        }
    }
    let mut stream = stream.unwrap();

    stream
        .write_all(serde_json::to_string(&msg)?.as_bytes())
        .await?;
    stream.shutdown().await?;

    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;

    let result: Value = serde_json::from_slice(&buf).context("Invalid response from ext-server")?;

    if let Some(err) = result.get("error").and_then(|v| v.as_str()) {
        bail!("{err}");
    }

    print_result(command, &result);
    Ok(())
}

fn stop_server() -> Result<()> {
    let pid_file = pid_path();
    if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            unsafe { libc::kill(pid, libc::SIGTERM) };
            let _ = std::fs::remove_file(&pid_file);
            println!("Stopped ext-server (PID {pid})");
            return Ok(());
        }
    }
    println!("No ext-server running");
    Ok(())
}

fn show_status() -> Result<()> {
    if is_server_running() {
        let pid = std::fs::read_to_string(pid_path())
            .unwrap_or_default()
            .trim()
            .to_string();
        println!("ext-server running (PID {pid})");
    } else {
        println!("ext-server not running");
    }
    Ok(())
}

fn build_command(command: &str, args: &[String], default_tab: Option<u64>) -> Result<Value> {
    // Explicit tab id in subcommand args wins over global `--tab`.
    fn tab_from_arg_or_default(explicit: Option<u64>, default_tab: Option<u64>) -> Option<u64> {
        explicit.or(default_tab)
    }

    match command {
        "tabs" => Ok(json!({"command": "tabs"})),
        "read" => {
            let tab_id = tab_from_arg_or_default(
                args.first().and_then(|s| s.parse::<u64>().ok()),
                default_tab,
            );
            let mut cmd = json!({"command": "read"});
            if let Some(id) = tab_id {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "screenshot" => {
            let tab_id = tab_from_arg_or_default(
                args.first().and_then(|s| s.parse::<u64>().ok()),
                default_tab,
            );
            let mut cmd = json!({"command": "screenshot"});
            if let Some(id) = tab_id {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "click" => {
            let target = args
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("Usage: sidekar ext click <selector|text:...>"))?;
            let mut cmd = json!({"command": "click", "target": target});
            if let Some(id) = default_tab {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "type" => {
            if args.len() < 2 {
                bail!("Usage: sidekar ext type <selector> <text>");
            }
            let mut cmd =
                json!({"command": "type", "selector": args[0], "text": args[1..].join(" ")});
            if let Some(id) = default_tab {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "paste" => {
            let mut html: Option<String> = None;
            let mut text: Option<String> = None;
            let mut selector: Option<String> = None;
            let mut plain_parts = Vec::new();
            let mut i = 0;
            while i < args.len() {
                match args[i].as_str() {
                    "--html" => {
                        i += 1;
                        let value = args.get(i).cloned().context(
                            "Usage: sidekar ext paste [--html <html>] [--text <text>] [--selector <selector>]",
                        )?;
                        html = Some(value);
                    }
                    "--text" => {
                        i += 1;
                        let value = args.get(i).cloned().context(
                            "Usage: sidekar ext paste [--html <html>] [--text <text>] [--selector <selector>]",
                        )?;
                        text = Some(value);
                    }
                    "--selector" => {
                        i += 1;
                        let value = args.get(i).cloned().context(
                            "Usage: sidekar ext paste [--html <html>] [--text <text>] [--selector <selector>]",
                        )?;
                        selector = Some(value);
                    }
                    other => plain_parts.push(other.to_string()),
                }
                i += 1;
            }
            if text.is_none() && !plain_parts.is_empty() {
                text = Some(plain_parts.join(" "));
            }
            if text.is_none() && html.is_some() {
                text = html.clone();
            }
            if text.as_deref().unwrap_or("").is_empty() && html.as_deref().unwrap_or("").is_empty()
            {
                bail!(
                    "Usage: sidekar ext paste [--html <html>] [--text <text>] [--selector <selector>]"
                );
            }
            let mut cmd = json!({"command": "paste", "text": text.unwrap_or_default()});
            if let Some(html) = html {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("html".into(), json!(html));
            }
            if let Some(selector) = selector {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("selector".into(), json!(selector));
            }
            if let Some(id) = default_tab {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "setvalue" => {
            if args.len() < 2 {
                bail!("Usage: sidekar ext setvalue <selector> <text>");
            }
            let mut cmd =
                json!({"command": "setvalue", "selector": args[0], "text": args[1..].join(" ")});
            if let Some(id) = default_tab {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "axtree" => {
            let tab_id = tab_from_arg_or_default(
                args.first().and_then(|s| s.parse::<u64>().ok()),
                default_tab,
            );
            let mut cmd = json!({"command": "axtree"});
            if let Some(id) = tab_id {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "eval" => {
            let code = args.join(" ");
            if code.is_empty() {
                bail!("Usage: sidekar ext eval <javascript>");
            }
            let mut cmd = json!({"command": "eval", "code": code});
            if let Some(id) = default_tab {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "evalpage" => {
            let code = args.join(" ");
            if code.is_empty() {
                bail!("Usage: sidekar ext evalpage <javascript>");
            }
            let mut cmd = json!({"command": "evalpage", "code": code});
            if let Some(id) = default_tab {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "navigate" => {
            if args.is_empty() {
                bail!("Usage: sidekar ext navigate <url> [tab_id]");
            }
            let url = &args[0];
            let tab_id = tab_from_arg_or_default(
                args.get(1).and_then(|s| s.parse::<u64>().ok()),
                default_tab,
            );
            let mut cmd = json!({"command": "navigate", "url": url});
            if let Some(id) = tab_id {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "newtab" => {
            let url = args
                .first()
                .cloned()
                .unwrap_or_else(|| "about:blank".to_string());
            Ok(json!({"command": "newtab", "url": url}))
        }
        "close" => {
            let tab_id = tab_from_arg_or_default(
                args.first().and_then(|s| s.parse::<u64>().ok()),
                default_tab,
            );
            let mut cmd = json!({"command": "close"});
            if let Some(id) = tab_id {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        "scroll" => {
            let direction = args.first().map(|s| s.as_str()).unwrap_or("down");
            let mut cmd = json!({"command": "scroll", "direction": direction});
            if let Some(id) = default_tab {
                cmd.as_object_mut()
                    .unwrap()
                    .insert("tabId".into(), json!(id));
            }
            Ok(cmd)
        }
        _ => bail!(
            "Unknown ext command: {command}\nAvailable: tabs, read, screenshot, click, type, paste, setvalue, axtree, eval, evalpage, navigate, newtab, close, scroll, status, stop"
        ),
    }
}

fn print_result(command: &str, result: &Value) {
    match command {
        "tabs" => {
            if let Some(tabs) = result.get("tabs").and_then(|v| v.as_array()) {
                for tab in tabs {
                    let id = tab.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                    let title = tab.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    let url = tab.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let active = tab.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
                    let marker = if active { " *" } else { "" };
                    println!("[{id}]{marker} {title}");
                    println!("  {url}");
                }
                println!("\n{} tab(s)", tabs.len());
            }
        }
        "read" => {
            if let Some(title) = result.get("title").and_then(|v| v.as_str()) {
                println!("--- {} ---", title);
            }
            if let Some(url) = result.get("url").and_then(|v| v.as_str()) {
                println!("{url}\n");
            }
            if let Some(text) = result.get("text").and_then(|v| v.as_str()) {
                println!("{text}");
            }
        }
        "screenshot" => {
            if let Some(data_url) = result.get("screenshot").and_then(|v| v.as_str()) {
                if let Some(b64) = data_url.strip_prefix("data:image/jpeg;base64,") {
                    if let Ok(bytes) =
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
                    {
                        let path =
                            format!("/tmp/sidekar-ext-screenshot-{}.jpg", rand::random::<u32>());
                        if std::fs::write(&path, &bytes).is_ok() {
                            println!("Screenshot saved: {path}");
                            return;
                        }
                    }
                }
                println!("Screenshot captured ({} bytes)", data_url.len());
            }
        }
        "axtree" => {
            if let Some(title) = result.get("title").and_then(|v| v.as_str()) {
                println!("--- {} ---", title);
            }
            if let Some(elements) = result.get("elements").and_then(|v| v.as_array()) {
                for el in elements {
                    let r = el.get("ref").and_then(|v| v.as_u64()).unwrap_or(0);
                    let role = el.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    let name = el.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    println!("[{r}] {role}: {name}");
                }
                println!("\n{} interactive element(s)", elements.len());
            }
        }
        "navigate" => {
            if let Some(title) = result.get("title").and_then(|v| v.as_str()) {
                println!("--- {} ---", title);
            }
            if let Some(url) = result.get("url").and_then(|v| v.as_str()) {
                println!("{url}");
            }
        }
        "newtab" => {
            let id = result.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
            let title = result.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let url = result.get("url").and_then(|v| v.as_str()).unwrap_or("");
            println!("Opened tab [{id}] {title}");
            println!("  {url}");
        }
        "close" => {
            let id = result.get("tabId").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("Closed tab [{id}]");
        }
        "paste" => {
            let mode = result
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let len = result.get("length").and_then(|v| v.as_u64()).unwrap_or(0);
            let verified = result
                .get("verified")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if verified {
                println!("Pasted {len} chars via {mode}");
            } else {
                println!("Paste attempted via {mode} ({len} chars, not verified)");
            }
            if let Some(err) = result.get("clipboard_error").and_then(|v| v.as_str()) {
                println!("Clipboard write warning: {err}");
            }
        }
        "setvalue" => {
            let mode = result
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let len = result.get("length").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("Set value via {mode} ({len} chars)");
        }
        "evalpage" => {
            if let Some(value) = result.get("result") {
                if value.is_string() {
                    println!("{}", value.as_str().unwrap_or_default());
                } else {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(value).unwrap_or_default()
                    );
                }
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(result).unwrap_or_default()
                );
            }
        }
        _ => {
            println!(
                "{}",
                serde_json::to_string_pretty(result).unwrap_or_default()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Native Messaging Host
// ---------------------------------------------------------------------------

const NATIVE_HOST_NAME: &str = "dev.sidekar";

/// Run as a native messaging host. Reads JSON messages from stdin (length-prefixed),
/// processes commands, and writes responses to stdout (length-prefixed).
pub fn run_native_host() -> Result<()> {
    use std::io::{Read, Write};

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();

    loop {
        // Read 4-byte length prefix (little-endian)
        let mut len_buf = [0u8; 4];
        if stdin.read_exact(&mut len_buf).is_err() {
            break; // EOF or error, exit cleanly
        }
        let len = u32::from_le_bytes(len_buf) as usize;

        if len == 0 || len > 1024 * 1024 {
            break; // Invalid length
        }

        // Read the message
        let mut msg_buf = vec![0u8; len];
        if stdin.read_exact(&mut msg_buf).is_err() {
            break;
        }

        // Parse and handle
        let response = match serde_json::from_slice::<Value>(&msg_buf) {
            Ok(msg) => handle_native_message(&msg),
            Err(e) => json!({"error": format!("Invalid JSON: {e}")}),
        };

        // Write response with length prefix
        let response_bytes = serde_json::to_vec(&response).unwrap_or_default();
        let response_len = (response_bytes.len() as u32).to_le_bytes();
        let _ = stdout.write_all(&response_len);
        let _ = stdout.write_all(&response_bytes);
        let _ = stdout.flush();
    }

    Ok(())
}

fn handle_native_message(msg: &Value) -> Value {
    let cmd = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match cmd {
        "get_config" => {
            // Return port and daemon status
            let port = std::env::var("SIDEKAR_EXT_PORT")
                .ok()
                .and_then(|v| v.parse::<u16>().ok())
                .unwrap_or(DEFAULT_PORT);
            let running = crate::daemon::is_running();
            let cli_logged_in = crate::auth::auth_token().is_some();
            json!({
                "port": port,
                "running": running,
                "cli_logged_in": cli_logged_in
            })
        }
        "ensure_server" => {
            // Start daemon if not running, return port
            match crate::daemon::ensure_running() {
                Ok(()) => {
                    let port = std::env::var("SIDEKAR_EXT_PORT")
                        .ok()
                        .and_then(|v| v.parse::<u16>().ok())
                        .unwrap_or(DEFAULT_PORT);
                    let cli_logged_in = crate::auth::auth_token().is_some();
                    json!({"port": port, "started": true, "cli_logged_in": cli_logged_in})
                }
                Err(e) => json!({"error": format!("{e}")}),
            }
        }
        "ping" => json!({"pong": true}),
        _ => json!({"error": format!("Unknown command: {cmd}")}),
    }
}

/// Install the native messaging host manifest for Chrome.
fn install_native_host_impl(extension_id: Option<&str>, verbose: bool) -> Result<()> {
    let exe_path = std::env::current_exe().context("Cannot determine sidekar executable path")?;

    // Create a wrapper script that calls sidekar with the native-messaging-host command
    let wrapper_path = exe_path
        .parent()
        .unwrap_or(std::path::Path::new("/usr/local/bin"))
        .join("sidekar-native-host");

    let wrapper_script = format!(
        "#!/bin/bash\nexec \"{}\" native-messaging-host \"$@\"\n",
        exe_path.display()
    );
    std::fs::write(&wrapper_path, &wrapper_script).with_context(|| {
        format!(
            "Failed to write wrapper script to {}",
            wrapper_path.display()
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper_path, std::fs::Permissions::from_mode(0o755))?;
    }

    // Use provided extension ID or the official published ID
    let ext_id = extension_id.unwrap_or("ieggclnoffcnljcjeadgogpfbnhogncc");

    let manifest = json!({
        "name": NATIVE_HOST_NAME,
        "description": "Sidekar native messaging host",
        "path": wrapper_path.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{ext_id}/")]
    });

    // Determine manifest path based on OS
    #[cfg(target_os = "macos")]
    let manifest_dir = dirs::home_dir()
        .ok_or_else(|| anyhow!("Cannot find home directory"))?
        .join("Library/Application Support/Google/Chrome/NativeMessagingHosts");

    #[cfg(target_os = "linux")]
    let manifest_dir = dirs::home_dir()
        .ok_or_else(|| anyhow!("Cannot find home directory"))?
        .join(".config/google-chrome/NativeMessagingHosts");

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    bail!("Native messaging host installation not supported on this OS");

    std::fs::create_dir_all(&manifest_dir)
        .context("Failed to create NativeMessagingHosts directory")?;

    let manifest_path = manifest_dir.join(format!("{NATIVE_HOST_NAME}.json"));
    let manifest_json = serde_json::to_string_pretty(&manifest)?;

    std::fs::write(&manifest_path, &manifest_json)
        .with_context(|| format!("Failed to write {}", manifest_path.display()))?;

    if verbose {
        println!("Installed native messaging host manifest:");
        println!("  {}", manifest_path.display());
        println!();
        println!("Manifest contents:");
        println!("{manifest_json}");
    }

    Ok(())
}

pub fn install_native_host(extension_id: Option<&str>) -> Result<()> {
    install_native_host_impl(extension_id, true)
}

pub(crate) fn install_native_host_quiet(extension_id: Option<&str>) -> Result<()> {
    install_native_host_impl(extension_id, false)
}
