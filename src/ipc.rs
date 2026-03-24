//! Inter-process communication for agent messaging.
//!
//! Provides tmux pane detection, message paste,
//! a JSON-RPC socket listener, and a socket client for cross-session
//! agent discovery and messaging.

use crate::broker;
use crate::message::AgentId;
use crate::*;
use std::io::{Read as IoRead, Write as IoWrite};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixListener;
use std::sync::Arc;

/// Set SO_RCVTIMEO on a UnixListener so accept() times out periodically.
fn nix_set_socket_timeout(listener: &UnixListener, timeout: std::time::Duration) -> Result<()> {
    let fd = listener.as_raw_fd();
    let tv = libc::timeval {
        tv_sec: timeout.as_secs() as libc::time_t,
        tv_usec: timeout.subsec_micros() as libc::suseconds_t,
    };
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &tv as *const libc::timeval as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        bail!(
            "setsockopt SO_RCVTIMEO failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

/// How the socket listener delivers inbound messages.
pub enum InputSink {
    /// Paste into a tmux pane (display pane ID).
    TmuxPane(String),
    /// Write directly to a PTY master fd.
    PtyFd(Arc<OwnedFd>),
}

/// Write an entire buffer to a raw fd, retrying on EINTR and short writes.
fn write_all_raw(fd: i32, mut buf: &[u8]) -> Result<()> {
    let mut retries = 0u32;
    while !buf.is_empty() {
        let n = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
        if n > 0 {
            buf = &buf[n as usize..];
            retries = 0;
        } else if n == 0 {
            bail!("write to fd returned 0");
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
            bail!("write to fd failed: {err}");
        }
    }
    Ok(())
}

/// Write a message to a PTY master fd (for InputSink::PtyFd).
/// Writes the text first, pauses briefly, then sends CR separately —
/// matching the tmux paste-buffer + send-keys pattern that TUI apps expect.
fn write_to_pty_fd(fd: &OwnedFd, message: &str) -> Result<()> {
    let raw_fd = fd.as_raw_fd();

    // Write message text
    write_all_raw(raw_fd, message.as_bytes())?;

    // Brief pause so the TUI processes the text before Enter arrives
    std::thread::sleep(std::time::Duration::from_millis(150));

    // Send Enter (CR) as a separate write
    write_all_raw(raw_fd, b"\r")?;

    Ok(())
}

const IPC_MAX_MESSAGE_SIZE: usize = 4096;
const IPC_PASTE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(5);
const IPC_PROTOCOL_VERSION: u32 = 1;

// --- Pane detection ---

/// Detected tmux pane info.
pub struct DetectedPane {
    /// Unique pane ID, e.g., "%42"
    pub unique_id: String,
    /// Display pane ID, e.g., "0:0.1"
    pub display_id: String,
    /// Session name
    pub session: String,
}

/// Walk the process tree to find which tmux pane we're in.
pub fn detect_tmux_pane() -> Option<DetectedPane> {
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{pane_pid}\t#{pane_id}\t#{session_name}:#{window_index}.#{pane_index}\t#{session_name}",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    // Map pid -> (unique_id, display_id, session)
    let mut pane_map: HashMap<String, (String, String, String)> = HashMap::new();
    for line in text.trim().lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() == 4 {
            pane_map.insert(
                parts[0].to_string(),
                (
                    parts[1].to_string(),
                    parts[2].to_string(),
                    parts[3].to_string(),
                ),
            );
        }
    }

    let mut pid = std::process::id();
    loop {
        if pid <= 1 {
            break;
        }
        if let Some((unique_id, display_id, session)) = pane_map.get(&pid.to_string()) {
            return Some(DetectedPane {
                unique_id: unique_id.clone(),
                display_id: display_id.clone(),
                session: session.clone(),
            });
        }
        // Walk up to parent
        match Command::new("ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse::<u32>()
                    .ok()
            }) {
            Some(ppid) if ppid != pid => pid = ppid,
            _ => break,
        }
    }
    None
}

/// Read agent nick and name from the durable broker. Returns (nick, name).
fn read_pane_identity(pane: &str) -> (Option<String>, Option<String>) {
    match broker::agent_for_pane_unique(pane) {
        Ok(Some(agent)) => (agent.id.nick, Some(agent.id.name)),
        _ => (None, None),
    }
}

// --- Tmux paste ---

pub fn capture_pane(pane: &str) -> String {
    Command::new("tmux")
        .args(["capture-pane", "-t", pane, "-p"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

fn do_send(pane: &str, sanitized: &str) -> Result<bool> {
    let before = capture_pane(pane);

    // Paste text via tmux paste-buffer (handles special chars better than send-keys),
    // then send Enter via send-keys (some CLIs don't submit on pasted newlines).
    let mut child = Command::new("tmux")
        .args(["load-buffer", "-"])
        .stdin(Stdio::piped())
        .spawn()
        .context("failed to run tmux load-buffer")?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(sanitized.as_bytes());
    }
    let status = child.wait().context("tmux load-buffer failed")?;
    if !status.success() {
        bail!("tmux load-buffer failed");
    }

    let status = Command::new("tmux")
        .args(["paste-buffer", "-t", pane])
        .status()
        .context("failed to run tmux paste-buffer")?;
    if !status.success() {
        bail!("tmux paste-buffer failed");
    }

    std::thread::sleep(std::time::Duration::from_millis(200));

    // Send Enter separately — pasted newlines don't always trigger submission
    let _ = Command::new("tmux")
        .args(["send-keys", "-t", pane, "Enter"])
        .status();

    // Brief poll to detect obvious paste failures (for retry logic)
    for _ in 0..8 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let current = capture_pane(pane);
        if current != before {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Send a message to a tmux pane via paste-buffer. Sanitizes special characters
/// and retries once on failure.
pub fn send_to_pane(pane: &str, message: &str) -> Result<()> {
    let sanitized: String = message
        .replace('!', "\u{FF01}")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    // First attempt
    if do_send(pane, &sanitized)? {
        return Ok(());
    }

    // Retry once
    eprintln!("sidekar ipc: first send to {pane} saw no pane change, retrying...");
    let _ = do_send(pane, &sanitized)?;
    Ok(())
}

// --- Socket path ---

/// Per-user temporary directory for sockets. Prefers $TMPDIR (per-user on macOS),
/// falls back to /tmp.
fn socket_dir() -> std::path::PathBuf {
    std::env::var_os("TMPDIR")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
}

/// All directories to scan for agent sockets. Includes both $TMPDIR and /tmp
/// to discover agents that may have been started under either.
fn socket_scan_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = vec![socket_dir()];
    let fallback = std::path::PathBuf::from("/tmp");
    if !dirs.contains(&fallback) {
        dirs.push(fallback);
    }
    dirs
}

/// Socket path for this sidekar instance.
pub fn socket_path(unique_pane_id: &str) -> std::path::PathBuf {
    socket_dir().join(format!("sidekar-{unique_pane_id}.sock"))
}

/// Clean up a stale socket file.
fn cleanup_stale_socket(path: &std::path::Path) {
    if path.exists() {
        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => {
                eprintln!(
                    "sidekar ipc: socket {} is in use by another process",
                    path.display()
                );
            }
            Err(_) => {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

// --- Socket listener ---

/// Start the IPC socket listener in a background thread.
/// Returns the socket path for cleanup.
/// Shared shutdown flag for socket listener threads.
static LISTENER_SHUTDOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Signal all socket listener threads to stop.
pub fn shutdown_listeners() {
    LISTENER_SHUTDOWN.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn start_socket_listener(
    unique_pane_id: &str,
    display_pane: &str,
    session: &str,
    agent_name: &str,
    agent_nick: Option<&str>,
    sink: InputSink,
) -> Result<std::path::PathBuf> {
    let path = socket_path(unique_pane_id);
    cleanup_stale_socket(&path);

    let listener = UnixListener::bind(&path)
        .with_context(|| format!("failed to bind IPC socket at {}", path.display()))?;

    // Set a timeout so accept() unblocks periodically to check shutdown flag.
    listener.set_nonblocking(false)?;
    let _ = nix_set_socket_timeout(&listener, std::time::Duration::from_secs(1));

    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    let name = agent_name.to_string();
    let nick = agent_nick.map(|n| n.to_string());
    let sess = session.to_string();
    let pane = display_pane.to_string();

    LISTENER_SHUTDOWN.store(false, std::sync::atomic::Ordering::Relaxed);

    std::thread::spawn(move || {
        let mut last_paste = std::time::Instant::now() - IPC_PASTE_COOLDOWN;

        for stream in listener.incoming() {
            if LISTENER_SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let mut stream = match stream {
                Ok(s) => s,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue; // accept() timeout — loop to check shutdown flag
                }
                Err(e) => {
                    eprintln!("sidekar ipc: accept error: {e}");
                    continue;
                }
            };

            let mut buf = vec![0u8; IPC_MAX_MESSAGE_SIZE + 512];
            let n = match stream.read(&mut buf) {
                Ok(n) if n > 0 => n,
                _ => continue,
            };

            let request: serde_json::Value = match serde_json::from_slice(&buf[..n]) {
                Ok(v) => v,
                Err(_) => {
                    let err = json!({
                        "jsonrpc": "2.0",
                        "id": serde_json::Value::Null,
                        "error": { "code": -32700, "message": "Parse error" }
                    });
                    let _ = stream
                        .write_all(serde_json::to_string(&err).unwrap_or_default().as_bytes());
                    continue;
                }
            };

            let id = request.get("id").cloned();
            let method = request
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default();

            let response = match method {
                "who" => {
                    let mut result = json!({
                        "agent": name,
                        "session": sess,
                        "pane": pane,
                        "version": IPC_PROTOCOL_VERSION,
                        "type": "sidekar"
                    });
                    if let Some(ref n) = nick {
                        result["nick"] = json!(n);
                    }
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": result
                    })
                }
                "send" => {
                    let params = request.get("params").cloned().unwrap_or(Value::Null);
                    let message = params
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let from = params
                        .get("from")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");

                    if message.is_empty() {
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32602, "message": "Missing 'message' parameter" }
                        })
                    } else if message.len() > IPC_MAX_MESSAGE_SIZE {
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32602, "message": format!("Message exceeds {}B limit", IPC_MAX_MESSAGE_SIZE) }
                        })
                    } else {
                        // Rate limit pastes
                        let elapsed = last_paste.elapsed();
                        if elapsed < IPC_PASTE_COOLDOWN {
                            std::thread::sleep(IPC_PASTE_COOLDOWN - elapsed);
                        }

                        // Deliver via the configured sink
                        let delivery_result = match &sink {
                            InputSink::TmuxPane(_) => send_to_pane(&pane, message),
                            InputSink::PtyFd(fd) => write_to_pty_fd(fd, message),
                        };
                        match delivery_result {
                            Ok(()) => {
                                last_paste = std::time::Instant::now();
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": { "ok": true }
                                })
                            }
                            Err(e) => {
                                last_paste = std::time::Instant::now();
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "error": { "code": -32000, "message": format!("Delivery failed: {e}") }
                                })
                            }
                        }
                    }
                }
                _ => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("Method not found: {method}") }
                }),
            };

            let _ = stream.write_all(
                serde_json::to_string(&response)
                    .unwrap_or_default()
                    .as_bytes(),
            );
        }
    });

    Ok(path)
}

// --- Socket client ---

/// Query an agent's identity via its IPC socket.
pub fn ipc_query_who(path: &std::path::Path) -> Option<serde_json::Value> {
    let mut stream = std::os::unix::net::UnixStream::connect(path).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let request = json!({"jsonrpc": "2.0", "method": "who", "id": 1});
    stream
        .write_all(serde_json::to_string(&request).ok()?.as_bytes())
        .ok()?;
    let mut buf = vec![0u8; 1024];
    let n = stream.read(&mut buf).ok()?;
    let response: serde_json::Value = serde_json::from_slice(&buf[..n]).ok()?;
    response.get("result").cloned()
}

/// Send a message to an agent via its IPC socket.
pub fn ipc_send_message(socket_path: &std::path::Path, message: &str, from: &str) -> Result<()> {
    if message.len() > IPC_MAX_MESSAGE_SIZE {
        bail!(
            "Message too large ({} bytes, max {})",
            message.len(),
            IPC_MAX_MESSAGE_SIZE
        );
    }
    let mut stream = std::os::unix::net::UnixStream::connect(socket_path).with_context(|| {
        format!(
            "failed to connect to IPC socket at {}",
            socket_path.display()
        )
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "send",
        "params": { "message": message, "from": from },
        "id": 1
    });
    stream.write_all(serde_json::to_string(&request)?.as_bytes())?;

    let mut buf = vec![0u8; 1024];
    let n = stream
        .read(&mut buf)
        .context("no response from IPC socket")?;
    let response: serde_json::Value = serde_json::from_slice(&buf[..n])?;
    if let Some(err) = response.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        bail!("{msg}");
    }
    Ok(())
}

/// Parse an IPC `who` response (JSON value) into an [`AgentId`].
pub fn agent_id_from_who(info: &serde_json::Value) -> AgentId {
    AgentId {
        name: info
            .get("agent")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string(),
        nick: info.get("nick").and_then(Value::as_str).map(String::from),
        session: info
            .get("session")
            .and_then(Value::as_str)
            .map(String::from),
        pane: info.get("pane").and_then(Value::as_str).map(String::from),
        agent_type: info.get("type").and_then(Value::as_str).map(String::from),
    }
}

/// Discover all agents across sessions by scanning IPC sockets.
/// Scans both sidekar-% and agentbus-% socket patterns.
pub fn discover_all_agents() -> Vec<(std::path::PathBuf, AgentId)> {
    let mut agents = Vec::new();
    let mut seen_paths: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    for dir in socket_scan_dirs() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let is_agent_socket = (name.starts_with("sidekar-")
                    || name.starts_with("agentbus-"))
                    && name.ends_with(".sock");
                if is_agent_socket {
                    let path = entry.path();
                    if !seen_paths.insert(path.clone()) {
                        continue;
                    }
                    if let Some(info) = ipc_query_who(&path) {
                        agents.push((path, agent_id_from_who(&info)));
                    } else {
                        // Stale socket — clean up
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
    }
    agents
}

/// Find an agent's socket by name or nickname across all sessions.
pub fn find_agent_socket(target: &str) -> Option<std::path::PathBuf> {
    discover_all_agents()
        .into_iter()
        .find(|(_, id)| id.name == target || id.nick.as_deref() == Some(target))
        .map(|(path, _)| path)
}

/// Find the IPC socket associated with a specific tmux pane.
/// Checks both sidekar-% and agentbus-% patterns.
pub fn find_socket_for_pane(pane_unique_id: &str) -> Option<std::path::PathBuf> {
    // Check known socket paths in all scan dirs
    for dir in socket_scan_dirs() {
        let sidekar_path = dir.join(format!("sidekar-{pane_unique_id}.sock"));
        if sidekar_path.exists() {
            return Some(sidekar_path);
        }
        let agentbus_path = dir.join(format!("agentbus-{pane_unique_id}.sock"));
        if agentbus_path.exists() {
            return Some(agentbus_path);
        }
    }
    // Scan all sockets and query for the pane
    discover_all_agents()
        .into_iter()
        .find(|(_, id)| {
            id.pane
                .as_deref()
                .map(|p| p == pane_unique_id || p.ends_with(&format!(".{}", pane_unique_id)))
                .unwrap_or(false)
        })
        .map(|(path, _)| path)
}

// --- MCP tool handlers ---

/// Handle the `who` tool — discover all agents.
pub fn cmd_who(ctx: &mut crate::AppContext) -> Result<()> {
    let agents = discover_all_agents();
    if agents.is_empty() {
        out!(
            ctx,
            "No agents discovered. (Are any agents running in tmux with sidekar?)"
        );
        return Ok(());
    }
    let mut lines = Vec::new();
    for (path, id) in &agents {
        let session = id.session.as_deref().unwrap_or("?");
        let pane = id.pane.as_deref().unwrap_or("?");
        let agent_type = id.agent_type.as_deref().unwrap_or("unknown");
        let socket = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        let nick_str = id
            .nick
            .as_deref()
            .map(|n| format!(" \"{n}\""))
            .unwrap_or_default();
        let cwd_str = broker::find_agent(&id.name, None)
            .ok()
            .flatten()
            .and_then(|a| a.cwd)
            .map(|c| format!(", cwd: {c}"))
            .unwrap_or_default();
        lines.push(format!("- {}{nick_str} (session \"{session}\", pane {pane}, type: {agent_type}, socket: {socket}{cwd_str})", id.name));
    }
    out!(ctx, "Agents discovered:\n{}", lines.join("\n"));
    Ok(())
}

/// Handle the `bus_send` tool — send a message to an agent by name.
pub fn cmd_send_message(ctx: &mut crate::AppContext, to: &str, message: &str) -> Result<()> {
    let pane = detect_tmux_pane();
    let (my_nick, my_name) = if let Some(ref p) = pane {
        let (nick, name) = read_pane_identity(&p.unique_id);
        (
            nick,
            name.unwrap_or_else(|| format!("sidekar@{}", p.display_id)),
        )
    } else {
        (None, "sidekar".to_string())
    };
    let my_id = AgentId {
        name: my_name,
        nick: my_nick,
        session: None,
        pane: None,
        agent_type: Some("sidekar".into()),
    };

    match find_agent_socket(to) {
        Some(socket_path) => {
            let full_message = format!("[message from {}]: {message}", my_id.display_name());
            ipc_send_message(&socket_path, &full_message, &my_id.name)?;
            out!(ctx, "Message sent to {to}.");
            Ok(())
        }
        None => {
            let agents = discover_all_agents();
            let available: Vec<String> = agents.iter().map(|(_, id)| id.display_name()).collect();
            if available.is_empty() {
                bail!("Agent \"{to}\" not found. No agents discovered.");
            } else {
                bail!(
                    "Agent \"{to}\" not found. Available: {}",
                    available.join(", ")
                );
            }
        }
    }
}
