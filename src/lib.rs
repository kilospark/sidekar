pub use anyhow::{Context, Result, anyhow, bail};
pub use base64::Engine;
pub use fs2::FileExt;
pub use futures_util::{SinkExt, StreamExt};
pub use rand::RngCore;
pub use reqwest::Client;
pub use serde_json::{Value, json};
pub use std::collections::{HashMap, HashSet, VecDeque};
pub use std::env;
pub use std::fmt::Write as _;
pub use std::fs;
pub use std::net::TcpListener;
pub use std::path::{Path, PathBuf};
pub use std::process::{Command, Stdio};
pub use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
pub use tokio::time::{sleep, timeout};
pub use tokio_tungstenite::tungstenite::protocol::Message;

static CDP_SEND_TIMEOUT_SECS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(60);

pub fn set_cdp_timeout_secs(secs: u64) {
    CDP_SEND_TIMEOUT_SECS.store(secs, std::sync::atomic::Ordering::SeqCst);
}

fn cdp_send_timeout() -> Duration {
    Duration::from_secs(CDP_SEND_TIMEOUT_SECS.load(std::sync::atomic::Ordering::SeqCst))
}

#[cfg(test)]
pub(crate) fn test_home_lock() -> &'static std::sync::Mutex<()> {
    use std::sync::{Mutex, OnceLock};

    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

const MAX_PENDING_EVENTS: usize = 1000;

#[macro_export]
macro_rules! out {
    ($ctx:expr, $($arg:tt)*) => {{
        use std::fmt::Write;
        let _ = writeln!($ctx.output, $($arg)*);
    }};
}

/// Structured warning log to stderr. Prefixed with "sidekar:" for grepability.
#[macro_export]
macro_rules! wlog {
    ($($arg:tt)*) => {{
        eprintln!("sidekar: {}", format!($($arg)*));
    }};
}

pub mod api_client;
pub mod auth;
pub mod broker;
pub mod bus;

pub mod agent;
pub mod providers;
pub mod repl;
pub mod cdp_proxy;
pub mod cli;
pub mod commands;
pub mod config;
pub mod daemon;
pub mod desktop;
pub mod ext;
pub mod ipc;
pub mod memory;
pub mod message;
pub mod pakt;
pub mod poller;
pub mod pty;
pub mod repo;
pub mod rtk;
pub mod scripts;
pub mod scope;
pub mod skill;
pub mod tasks;
pub mod transport;
pub mod tunnel;
pub mod types;
pub mod utils;

pub use cli::{
    canonical_command_name, command_handler, command_requires_session,
    command_should_auto_launch_browser, is_ext_routable_command, is_known_command,
    removed_command_replacement,
};
pub use scripts::*;
pub use types::*;
pub use utils::*;

/// Sanitize a string for use in filenames (replace /, \, : with -; collapse -- to -).
pub fn sanitize_for_filename(s: &str) -> String {
    let replaced: String = s
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == ':' {
                '-'
            } else {
                c
            }
        })
        .collect();
    // Collapse consecutive hyphens
    let mut result = String::with_capacity(replaced.len());
    for c in replaced.chars() {
        if c == '-' && result.ends_with('-') {
            continue;
        }
        result.push(c);
    }
    result
}

pub const DEFAULT_CDP_PORT: u16 = 9222;
pub const DEFAULT_CDP_HOST: &str = "127.0.0.1";
pub const CACHE_TTL_MS: i64 = 48 * 60 * 60 * 1000;
pub const CACHE_MAX_ENTRIES: usize = 100;

pub struct AppContext {
    pub current_session_id: Option<String>,
    pub cdp_port: u16,
    pub cdp_host: String,
    pub launch_browser_name: Option<String>,
    pub http: Client,
    pub output: String,
    pub session_id: String,
    pub tool_counts: std::collections::HashMap<String, u64>,
    pub session_start: std::time::Instant,
    pub isolated: bool,
    pub current_profile: String,
    /// Override active tab — connects directly to this tab ID, bypassing session ownership.
    pub override_tab_id: Option<String>,
    /// Browser launched in headless mode — skip window management operations.
    pub headless: bool,
}

impl AppContext {
    pub fn new() -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .context("failed to initialize HTTP client")?;
        let ctx = Self {
            current_session_id: None,
            cdp_port: DEFAULT_CDP_PORT,
            cdp_host: DEFAULT_CDP_HOST.to_string(),
            launch_browser_name: None,
            http,
            output: String::new(),
            session_id: {
                let mut bytes = [0u8; 16];
                rand::rng().fill_bytes(&mut bytes);
                bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
            },
            tool_counts: std::collections::HashMap::new(),
            session_start: std::time::Instant::now(),
            isolated: false,
            current_profile: "default".to_string(),
            override_tab_id: None,
            headless: false,
        };
        // Ensure persistent data directories exist
        if let Err(e) = fs::create_dir_all(ctx.data_dir()) {
            wlog!("failed creating data dir: {e}");
        }
        if let Err(e) = fs::create_dir_all(ctx.chrome_profile_dir()) {
            wlog!("failed creating profile dir: {e}");
        }
        Ok(ctx)
    }

    pub fn drain_output(&mut self) -> String {
        std::mem::take(&mut self.output)
    }

    /// Persistent data directory: ~/.sidekar/
    pub fn data_dir(&self) -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".sidekar")
    }

    /// Ephemeral temp directory (screenshots, PDFs, network captures)
    pub fn tmp_dir(&self) -> PathBuf {
        env::temp_dir()
    }

    pub fn last_session_file(&self) -> PathBuf {
        // Inside a PTY, use a per-agent session file so multiple agents
        // don't clobber each other's session pointers.
        if let Ok(agent_name) = env::var("SIDEKAR_AGENT_NAME") {
            if !agent_name.is_empty() {
                let safe_name = sanitize_for_filename(&agent_name);
                return self.data_dir().join(format!("last-session-{safe_name}"));
            }
        }
        self.data_dir().join("last-session")
    }

    pub fn session_state_file(&self, session_id: &str) -> PathBuf {
        self.data_dir().join(format!("state-{session_id}.json"))
    }

    pub fn command_file(&self, session_id: &str) -> PathBuf {
        self.tmp_dir()
            .join(format!("sidekar-command-{session_id}.json"))
    }

    pub fn chrome_profile_dir(&self) -> PathBuf {
        self.data_dir().join("profiles").join("default")
    }

    pub fn chrome_port_file(&self) -> PathBuf {
        self.data_dir().join("chrome-port")
    }

    pub fn chrome_profile_dir_for(&self, profile: &str) -> PathBuf {
        self.data_dir().join("profiles").join(profile)
    }

    pub fn chrome_port_file_for(&self, profile: &str) -> PathBuf {
        self.chrome_profile_dir_for(profile).join("cdp-port")
    }

    pub fn action_cache_file(&self) -> PathBuf {
        self.data_dir().join("action-cache.json")
    }

    pub fn tab_locks_file(&self) -> PathBuf {
        self.data_dir().join("tab-locks.json")
    }

    pub fn default_download_dir(&self) -> PathBuf {
        self.data_dir().join("downloads")
    }

    pub fn network_log_file(&self) -> PathBuf {
        let sid = self
            .current_session_id
            .clone()
            .unwrap_or_else(|| "default".to_string());
        self.tmp_dir().join(format!("sidekar-network-{sid}.json"))
    }

    pub fn require_session_id(&self) -> Result<&str> {
        self.current_session_id
            .as_deref()
            .ok_or_else(|| anyhow!("No active session"))
    }

    pub fn set_current_session(&mut self, session_id: String) {
        self.current_session_id = Some(session_id);
    }

    pub fn load_session_state(&self) -> Result<SessionState> {
        let session_id = self.require_session_id()?.to_string();
        let path = self.session_state_file(&session_id);
        // Shared lock for consistent reads (exclusive lock taken by save)
        let lock_path = path.with_extension("lock");
        let _lock_file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&lock_path)
            .ok()
            .and_then(|f| { f.lock_shared().ok(); Some(f) });
        let mut state = if path.exists() {
            let content = fs::read_to_string(&path)
                .with_context(|| format!("failed reading {}", path.display()))?;
            match serde_json::from_str::<SessionState>(&content) {
                Ok(s) => s,
                Err(e) => {
                    wlog!("Corrupt session state at {}, resetting: {e}", path.display());
                    let _ = fs::remove_file(&path);
                    SessionState::default()
                }
            }
        } else {
            SessionState::default()
        };

        if state.session_id.is_empty() {
            state.session_id = session_id;
        }
        Ok(state)
    }

    pub fn save_session_state(&self, state: &SessionState) -> Result<()> {
        let session_id = self.require_session_id()?;
        let path = self.session_state_file(session_id);
        // File-level lock to prevent concurrent read-modify-write races
        let lock_path = path.with_extension("lock");
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&lock_path)
            .with_context(|| format!("failed opening lock {}", lock_path.display()))?;
        lock_file.lock_exclusive().ok();
        let result = atomic_write_json(&path, state);
        lock_file.unlock().ok();
        result
    }

    pub fn auto_discover_last_session(&mut self) -> Result<()> {
        let sid = fs::read_to_string(self.last_session_file())
            .context("No active session")?
            .trim()
            .to_string();
        if sid.is_empty() {
            bail!("No active session");
        }
        self.current_session_id = Some(sid);
        self.hydrate_connection_from_state()
    }

    pub fn hydrate_connection_from_state(&mut self) -> Result<()> {
        let state = self.load_session_state()?;
        if let Some(port) = state.port {
            self.cdp_port = port;
        }
        if let Some(host) = state.host {
            self.cdp_host = host;
        }
        Ok(())
    }
}

/// Atomic JSON write: serialize to temp file, then rename into place.
/// Prevents corruption from crashes mid-write and partial reads by other processes.
fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    // Use PID + random suffix to avoid races between concurrent writers
    let tmp = path.with_extension(format!(
        "tmp.{}.{:08x}",
        std::process::id(),
        rand::random::<u32>()
    ));
    let data = serde_json::to_string_pretty(value).context("failed serializing JSON")?;
    fs::write(&tmp, &data).with_context(|| format!("failed writing {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("failed renaming {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Direct WebSocket connection to Chrome's CDP (used when daemon unavailable).
pub struct DirectCdp {
    pub ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    pub next_id: u64,
    pub pending_events: VecDeque<Value>,
    /// If set, auto-handle JS dialogs via CDP Page.handleJavaScriptDialog.
    pub auto_dialog: Option<(bool, String)>, // (accept, promptText)
    closed: bool,
}

impl DirectCdp {
    pub async fn connect(ws_url: &str) -> Result<Self> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let request = ws_url
            .into_client_request()
            .with_context(|| format!("invalid CDP websocket URL: {ws_url}"))?;

        let host = request.uri().host().unwrap_or("127.0.0.1");
        let port = request.uri().port_u16().unwrap_or(9222);
        let addr = format!("{host}:{port}");

        // Connect asynchronously first (handles DNS resolution + non-blocking connect)
        let tcp_stream = tokio::net::TcpStream::connect(&addr)
            .await
            .with_context(|| format!("failed to connect CDP at {addr}"))?;

        // Apply TCP keepalive on the already-connected stream
        let sock_ref = socket2::SockRef::from(&tcp_stream);
        let keepalive = socket2::TcpKeepalive::new()
            .with_time(Duration::from_secs(30))
            .with_interval(Duration::from_secs(10));
        sock_ref.set_tcp_keepalive(&keepalive)?;

        let (ws, _) = tokio_tungstenite::client_async(request, tcp_stream)
            .await
            .with_context(|| format!("failed to connect CDP websocket: {ws_url}"))?;

        Ok(Self {
            ws,
            next_id: 1,
            pending_events: VecDeque::new(),
            auto_dialog: None,
            closed: false,
        })
    }

    async fn do_send(
        &mut self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let mut payload = json!({
            "id": id,
            "method": method,
            "params": params,
        });
        if let Some(sid) = session_id {
            payload["sessionId"] = json!(sid);
        }

        let context = if let Some(sid) = session_id {
            format!("failed to send CDP method {method} to session {sid}")
        } else {
            format!("failed to send CDP method {method}")
        };

        self.ws
            .send(Message::Text(payload.to_string().into()))
            .await
            .with_context(|| context)?;

        let timeout_duration = cdp_send_timeout();
        match timeout(timeout_duration, self.recv_response(id)).await {
            Ok(result) => result,
            Err(_) => bail!(
                "CDP method {method} timed out after {}s",
                timeout_duration.as_secs()
            ),
        }
    }

    /// Send a CDP command scoped to a specific session (target).
    pub async fn send_to_session(
        &mut self,
        method: &str,
        params: Value,
        session_id: &str,
    ) -> Result<Value> {
        self.do_send(method, params, Some(session_id)).await
    }

    pub async fn send(&mut self, method: &str, params: Value) -> Result<Value> {
        self.do_send(method, params, None).await
    }

    async fn recv_response(&mut self, id: u64) -> Result<Value> {
        while let Some(msg) = self.ws.next().await {
            let value = self
                .parse_ws_message(msg.context("CDP websocket read error")?)?
                .ok_or_else(|| anyhow!("WebSocket closed"))?;

            // Auto-handle JS dialogs at the CDP protocol level
            if value.get("method").and_then(Value::as_str) == Some("Page.javascriptDialogOpening") {
                if let Some((accept, prompt_text)) = &self.auto_dialog {
                    let dialog_id = self.next_id;
                    self.next_id += 1;
                    let mut params = json!({ "accept": *accept });
                    if !prompt_text.is_empty() {
                        params["promptText"] = Value::String(prompt_text.clone());
                    }
                    let payload = json!({
                        "id": dialog_id,
                        "method": "Page.handleJavaScriptDialog",
                        "params": params,
                    });
                    if let Err(e) = self
                        .ws
                        .send(Message::Text(payload.to_string().into()))
                        .await
                    {
                        wlog!("failed auto-handling dialog: {e}");
                    }
                    let dialog_type = value
                        .pointer("/params/type")
                        .and_then(Value::as_str)
                        .unwrap_or("dialog");
                    let msg_text = value
                        .pointer("/params/message")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    wlog!(
                        "Auto-{}ed {}: \"{}\"",
                        if *accept { "accept" } else { "dismiss" },
                        dialog_type,
                        msg_text
                    );
                    continue;
                }
            }

            if value.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = value.get("error") {
                    let message = err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Unknown CDP error");
                    let code = err.get("code").and_then(Value::as_i64).unwrap_or_default();
                    bail!("{message} ({code})");
                }
                return Ok(value.get("result").cloned().unwrap_or(Value::Null));
            }
            if value.get("method").is_some() {
                if self.pending_events.len() >= MAX_PENDING_EVENTS {
                    if self.pending_events.len() == MAX_PENDING_EVENTS {
                        wlog!(
                            "CDP event queue full ({MAX_PENDING_EVENTS}), dropping oldest events"
                        );
                    }
                    self.pending_events.pop_front();
                }
                self.pending_events.push_back(value);
            }
        }

        bail!("WebSocket closed")
    }

    pub async fn next_event(&mut self, wait: Duration) -> Result<Option<Value>> {
        if let Some(v) = self.pending_events.pop_front() {
            return Ok(Some(v));
        }

        let msg = match timeout(wait, self.ws.next()).await {
            Ok(maybe) => maybe,
            Err(_) => return Ok(None),
        };

        match msg {
            Some(raw) => self.parse_ws_message(raw.context("CDP websocket read error")?),
            None => Ok(None),
        }
    }

    pub fn parse_ws_message(&self, msg: Message) -> Result<Option<Value>> {
        match msg {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(&text)
                    .with_context(|| format!("invalid CDP JSON message: {text}"))?;
                Ok(Some(value))
            }
            Message::Binary(bin) => {
                let text = String::from_utf8_lossy(&bin);
                let value: Value = serde_json::from_str(&text)
                    .with_context(|| format!("invalid CDP JSON message: {text}"))?;
                Ok(Some(value))
            }
            Message::Close(_) => Ok(None),
            _ => Ok(Some(Value::Null)),
        }
    }

    pub async fn close(mut self) {
        self.closed = true;
        if let Err(e) = self.ws.close(None).await {
            wlog!("CDP close failed: {e}");
        }
    }
}

impl Drop for DirectCdp {
    fn drop(&mut self) {
        if !self.closed && std::env::var_os("SIDEKAR_DEBUG").is_some() {
            eprintln!("sidekar: DirectCdp dropped without close()");
        }
    }
}

// ---------------------------------------------------------------------------
// CdpClient — unified handle (direct WS or daemon proxy)
// ---------------------------------------------------------------------------

/// Unified CDP handle. Commands use this type; the connection may be
/// a direct WebSocket or proxied through the sidekar daemon.
pub enum CdpClient {
    Direct(DirectCdp),
    Proxied(cdp_proxy::DaemonCdpProxy),
}

impl CdpClient {
    /// Send a CDP command (no session scope).
    pub async fn send(&mut self, method: &str, params: Value) -> Result<Value> {
        match self {
            Self::Direct(d) => d.send(method, params).await,
            Self::Proxied(p) => p.send(method, params).await,
        }
    }

    /// Send a CDP command scoped to a session.
    pub async fn send_to_session(
        &mut self,
        method: &str,
        params: Value,
        session_id: &str,
    ) -> Result<Value> {
        match self {
            Self::Direct(d) => d.send_to_session(method, params, session_id).await,
            Self::Proxied(p) => p.send_to_session(method, params, session_id).await,
        }
    }

    /// Wait for a CDP event, with timeout.
    pub async fn next_event(&mut self, wait: Duration) -> Result<Option<Value>> {
        match self {
            Self::Direct(d) => d.next_event(wait).await,
            Self::Proxied(p) => p.next_event(wait).await,
        }
    }

    /// Close the connection. For proxied connections, this only closes the
    /// unix socket — the daemon keeps the WS connection alive for reuse.
    pub async fn close(self) {
        match self {
            Self::Direct(d) => d.close().await,
            Self::Proxied(p) => p.close().await,
        }
    }

    /// Access/set the auto-dialog handler.
    pub fn set_auto_dialog(&mut self, accept: bool, prompt_text: String) {
        match self {
            Self::Direct(d) => d.auto_dialog = Some((accept, prompt_text)),
            Self::Proxied(p) => p.auto_dialog = Some((accept, prompt_text)),
        }
    }

    /// Clear the auto-dialog handler.
    pub fn clear_auto_dialog(&mut self) {
        match self {
            Self::Direct(d) => d.auto_dialog = None,
            Self::Proxied(p) => p.auto_dialog = None,
        }
    }

    /// Access pending events buffer (for draining).
    pub fn pending_events_mut(&mut self) -> &mut VecDeque<Value> {
        match self {
            Self::Direct(d) => &mut d.pending_events,
            Self::Proxied(p) => &mut p.pending_events,
        }
    }
}

pub async fn open_cdp(ctx: &mut AppContext) -> Result<CdpClient> {
    match open_cdp_once(ctx).await {
        Ok(cdp) => Ok(cdp),
        Err(first_err) => {
            let msg = first_err.to_string();
            if msg.contains("WebSocket closed")
                || msg.contains("Connection refused")
                || msg.contains("failed to connect")
                || msg.contains("daemon")
            {
                wlog!("CDP connection failed ({msg}), retrying...");
                sleep(Duration::from_millis(500)).await;
                open_cdp_once(ctx)
                    .await
                    .with_context(|| format!("CDP retry also failed (original: {msg})"))
            } else {
                Err(first_err)
            }
        }
    }
}

async fn open_cdp_once(ctx: &mut AppContext) -> Result<CdpClient> {
    let tab = connect_to_tab(ctx).await?;
    if let Some(lock) = check_tab_lock(ctx, &tab.id)? {
        let sid = ctx.require_session_id()?;
        if lock.session_id != sid {
            let remaining = ((lock.expires - now_epoch_ms()).max(0) / 1000) as i64;
            bail!(
                "Tab is locked by session {} (expires in {}s). Use a different tab or wait.",
                lock.session_id,
                remaining
            );
        }
    }
    let ws_url = tab
        .web_socket_debugger_url
        .ok_or_else(|| anyhow!("No active tab for this session. Navigate to a URL first."))?;

    // Try daemon proxy first (persistent connection, keepalive)
    if daemon::is_running() {
        match cdp_proxy::DaemonCdpProxy::connect(&ws_url).await {
            Ok(proxy) => return Ok(CdpClient::Proxied(proxy)),
            Err(_) => {
                // Daemon unavailable or proxy failed — fall back to direct
            }
        }
    }

    // Direct connection (ephemeral per-call WS)
    Ok(CdpClient::Direct(DirectCdp::connect(&ws_url).await?))
}

pub async fn connect_to_tab(ctx: &mut AppContext) -> Result<DebugTab> {
    // --tab override: connect directly to the specified tab, bypassing session
    if let Some(ref target_id) = ctx.override_tab_id {
        let tabs = get_debug_tabs(ctx).await?;
        let tab = tabs
            .iter()
            .find(|t| t.id == *target_id)
            .cloned()
            .ok_or_else(|| anyhow!("Tab not found: {target_id}"))?;
        if tab.web_socket_debugger_url.is_none() {
            bail!("Tab {target_id} has no webSocketDebuggerUrl");
        }
        return Ok(tab);
    }

    let mut state = ctx.load_session_state()?;
    let tabs = get_debug_tabs(ctx).await?;

    // Prune stale tab IDs that no longer exist in Chrome
    let live_ids: HashSet<&str> = tabs.iter().map(|t| t.id.as_str()).collect();
    let before = state.tabs.len();
    state.tabs.retain(|id| live_ids.contains(id.as_str()));
    if state.tabs.len() < before {
        wlog!("Pruned {} stale tab ID(s) from session state", before - state.tabs.len());
    }

    let mut tab = None;
    if let Some(active_id) = state.active_tab_id.clone() {
        tab = tabs
            .iter()
            .find(|t| t.id == active_id && t.web_socket_debugger_url.is_some())
            .cloned();
        if tab.is_none() {
            for owned_id in &state.tabs {
                if let Some(found) = tabs
                    .iter()
                    .find(|t| t.id == *owned_id && t.web_socket_debugger_url.is_some())
                    .cloned()
                {
                    wlog!(
                        "Active tab {} lost, falling back to owned tab {}",
                        active_id,
                        found.id
                    );
                    tab = Some(found);
                    break;
                }
            }
        }
    }

    // If no owned tab was found, verify we're talking to the right Chrome
    // before creating a replacement. If the CDP port is serving another agent's
    // browser, bail instead of silently creating tabs in the wrong instance.
    let selected = match tab {
        Some(t) => t,
        None => {
            // Check if any of Chrome's current tabs are from a different session
            // (i.e., another agent owns tabs on this port). If so, refuse to create.
            let other_sessions = other_sessions_on_port(ctx, &state).await;
            if !other_sessions.is_empty() {
                bail!(
                    "All tabs for this session were closed. Other session(s) ({}) \
                     are using the same browser on port {}. \
                     Run `sidekar new-tab` to create a tab, or `sidekar launch` for a fresh browser.",
                    other_sessions.join(", "),
                    ctx.cdp_port,
                );
            }
            wlog!("Session tab lost — auto-creating replacement tab");
            let new_tab = create_new_tab(ctx, None).await?;
            state.tabs.push(new_tab.id.clone());
            new_tab
        }
    };
    state.active_tab_id = Some(selected.id.clone());
    ctx.save_session_state(&state)?;

    Ok(selected)
}

/// Check if any other sidekar sessions own tabs on the same CDP port.
/// Returns session IDs of any sessions whose tabs are found on this port.
async fn other_sessions_on_port(ctx: &AppContext, current_state: &SessionState) -> Vec<String> {
    let current_sid = current_state.session_id.clone();
    let data_dir = ctx.data_dir();

    // Scan all state files looking for sessions that share our port
    let mut others = Vec::new();
    if let Ok(entries) = fs::read_dir(&data_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if !name_str.starts_with("state-") || !name_str.ends_with(".json") {
                continue;
            }
            if let Ok(content) = fs::read_to_string(entry.path()) {
                if let Ok(state) = serde_json::from_str::<SessionState>(&content) {
                    if state.session_id == current_sid || state.session_id.is_empty() {
                        continue;
                    }
                    // Same port?
                    let port_matches = state.port.map_or(false, |p| p == ctx.cdp_port);
                    let host_matches = state
                        .host
                        .as_deref()
                        .map_or(true, |h| h == ctx.cdp_host);
                    if port_matches && host_matches && !state.tabs.is_empty() {
                        others.push(state.session_id);
                    }
                }
            }
        }
    }
    others
}

/// Verify Chrome's CDP is fully operational: HTTP + WebSocket + Browser.getVersion.
pub async fn verify_cdp_ready(ctx: &AppContext) -> Result<()> {
    let tabs = get_debug_tabs(ctx).await?;
    let tab = tabs.first().ok_or_else(|| anyhow!("No tabs available"))?;
    let ws_url = tab
        .web_socket_debugger_url
        .as_ref()
        .ok_or_else(|| anyhow!("No WebSocket URL"))?;
    let mut cdp = DirectCdp::connect(ws_url).await?;
    let result = cdp.send("Browser.getVersion", json!({})).await;
    cdp.close().await;
    result.map(|_| ())
}

pub async fn get_debug_tabs(ctx: &AppContext) -> Result<Vec<DebugTab>> {
    let body = http_get_text(ctx, "/json").await?;
    serde_json::from_str::<Vec<DebugTab>>(&body).context("Failed to parse Chrome debug info")
}

pub async fn create_new_tab(ctx: &AppContext, url: Option<&str>) -> Result<DebugTab> {
    let suffix = match url {
        Some(raw) if !raw.is_empty() => {
            // URL-encode to prevent Chrome from misinterpreting URL query params as HTTP params
            let encoded = urlencoding::encode(raw);
            format!("/json/new?{encoded}")
        }
        _ => "/json/new".to_string(),
    };
    let body = http_put_text(ctx, &suffix).await?;
    serde_json::from_str::<DebugTab>(&body).context("Failed to create new tab")
}

/// Create a tab in a new Chrome window using CDP Target.createTarget.
/// Requires an existing tab to connect via WebSocket.
pub async fn create_new_window(ctx: &AppContext, url: Option<&str>) -> Result<DebugTab> {
    // Find any existing tab to connect through
    let tabs = get_debug_tabs(ctx).await?;
    let any_tab = tabs
        .first()
        .ok_or_else(|| anyhow!("No existing tab to connect through"))?;
    let ws_url = any_tab
        .web_socket_debugger_url
        .as_ref()
        .ok_or_else(|| anyhow!("No WebSocket URL for existing tab"))?;
    let mut cdp = DirectCdp::connect(ws_url).await?;
    let result = cdp
        .send(
            "Target.createTarget",
            json!({
                "url": url.unwrap_or("about:blank"),
                "newWindow": true
            }),
        )
        .await?;

    let target_id = result
        .get("targetId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("No targetId in createTarget response"))?;

    cdp.close().await;

    // Fetch the full tab info via HTTP debug API (retry briefly for /json to catch up)
    for _ in 0..5 {
        let all_tabs = get_debug_tabs(ctx).await?;
        if let Some(tab) = all_tabs.into_iter().find(|t| t.id == target_id) {
            return Ok(tab);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    bail!("New window tab not found in tab list after retries")
}

/// Detect browser name from the debug port's /json/version endpoint.
/// More accurate than find_browser() when attaching to an existing browser.
pub async fn detect_browser_from_port(ctx: &AppContext) -> Option<String> {
    let body = http_get_text(ctx, "/json/version").await.ok()?;
    let info: Value = serde_json::from_str(&body).ok()?;
    let browser = info.get("Browser").and_then(Value::as_str).unwrap_or("");
    let user_agent = info.get("User-Agent").and_then(Value::as_str).unwrap_or("");

    // Browser field: "Chrome/131.0.6778.86", "HeadlessChrome/..."
    // User-Agent contains brand hints: "Edg/", "Brave/", "OPR/", "Vivaldi/"
    // Check user-agent first for more specific brands (they all report "Chrome/" in Browser)
    let name = if user_agent.contains("Edg/") {
        "Microsoft Edge"
    } else if user_agent.contains("Brave/") || user_agent.contains("brave") {
        "Brave Browser"
    } else if user_agent.contains("OPR/") || user_agent.contains("Opera") {
        "Opera"
    } else if user_agent.contains("Vivaldi/") {
        "Vivaldi"
    } else if user_agent.contains("Arc/") || user_agent.contains("arc ") {
        "Arc"
    } else if browser.starts_with("Chrome/") || browser.starts_with("HeadlessChrome/") {
        "Google Chrome"
    } else if browser.starts_with("Chromium/") {
        "Chromium"
    } else {
        return None;
    };
    Some(name.to_string())
}

/// Get the CDP window ID for a given target (tab).
pub async fn get_window_id_for_target(_ctx: &AppContext, tab_ws_url: &str) -> Result<i64> {
    let mut cdp = DirectCdp::connect(tab_ws_url).await?;
    let result = cdp.send("Browser.getWindowForTarget", json!({})).await?;
    cdp.close().await;
    result
        .get("windowId")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("No windowId in Browser.getWindowForTarget response"))
}

/// Minimize a specific Chrome window by its CDP window ID.
pub async fn minimize_window_by_id(
    _ctx: &AppContext,
    tab_ws_url: &str,
    window_id: i64,
) -> Result<()> {
    let mut cdp = DirectCdp::connect(tab_ws_url).await?;
    cdp.send(
        "Browser.setWindowBounds",
        json!({"windowId": window_id, "bounds": {"windowState": "minimized"}}),
    )
    .await?;
    cdp.close().await;
    Ok(())
}

/// Restore (un-minimize) a specific Chrome window by its CDP window ID.
pub async fn restore_window_by_id(
    _ctx: &AppContext,
    tab_ws_url: &str,
    window_id: i64,
) -> Result<()> {
    let mut cdp = DirectCdp::connect(tab_ws_url).await?;
    cdp.send(
        "Browser.setWindowBounds",
        json!({"windowId": window_id, "bounds": {"windowState": "normal"}}),
    )
    .await?;
    cdp.close().await;
    Ok(())
}

pub async fn http_get_text(ctx: &AppContext, path: &str) -> Result<String> {
    let url = format!("http://{}:{}{}", ctx.cdp_host, ctx.cdp_port, path);
    let resp = ctx
        .http
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url} failed"))?;
    timeout(Duration::from_secs(10), resp.text())
        .await
        .with_context(|| format!("GET {url} body read timed out"))?
        .with_context(|| format!("GET {url} body read failed"))
}

pub async fn http_put_text(ctx: &AppContext, path: &str) -> Result<String> {
    let url = format!("http://{}:{}{}", ctx.cdp_host, ctx.cdp_port, path);
    let resp = ctx
        .http
        .put(&url)
        .send()
        .await
        .with_context(|| format!("PUT {url} failed"))?;
    timeout(Duration::from_secs(10), resp.text())
        .await
        .with_context(|| format!("PUT {url} body read timed out"))?
        .with_context(|| format!("PUT {url} body read failed"))
}

pub fn check_js_error(result: &Value) -> Result<()> {
    if let Some(err) = result
        .pointer("/result/value/error")
        .and_then(Value::as_str)
    {
        bail!("{err}");
    }
    Ok(())
}

pub async fn runtime_evaluate(
    cdp: &mut CdpClient,
    expression: &str,
    return_by_value: bool,
    await_promise: bool,
) -> Result<Value> {
    runtime_evaluate_with_context(cdp, expression, return_by_value, await_promise, None).await
}

pub async fn runtime_evaluate_with_context(
    cdp: &mut CdpClient,
    expression: &str,
    return_by_value: bool,
    await_promise: bool,
    context_id: Option<i64>,
) -> Result<Value> {
    let mut params = json!({ "expression": expression });
    if return_by_value {
        params["returnByValue"] = Value::Bool(true);
    }
    if await_promise {
        params["awaitPromise"] = Value::Bool(true);
    }
    if let Some(id) = context_id {
        params["contextId"] = Value::from(id);
    }

    let result = cdp.send("Runtime.evaluate", params).await?;
    if let Some(details) = result.get("exceptionDetails") {
        let text = details
            .get("text")
            .and_then(Value::as_str)
            .or_else(|| {
                details
                    .get("exception")
                    .and_then(|ex| ex.get("description"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("Runtime evaluation failed");
        bail!("{text}");
    }

    Ok(result)
}

pub async fn get_frame_context_id(ctx: &AppContext, cdp: &mut CdpClient) -> Result<Option<i64>> {
    let state = ctx.load_session_state()?;
    if let Some(frame_id) = state.active_frame_id {
        let result = cdp
            .send(
                "Page.createIsolatedWorld",
                json!({
                    "frameId": frame_id,
                    "worldName": "sidekar",
                    "grantUniversalAccess": true
                }),
            )
            .await?;
        let context_id = result
            .get("executionContextId")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("Could not find execution context for selected frame"))?;
        return Ok(Some(context_id));
    }
    Ok(None)
}

pub async fn prepare_cdp(ctx: &mut AppContext, cdp: &mut CdpClient) -> Result<()> {
    let mut state = ctx.load_session_state()?;

    if let Some(handler) = state.dialog_handler.clone() {
        cdp.send("Page.enable", json!({})).await?;
        cdp.set_auto_dialog(handler.accept, handler.prompt_text);
        state.dialog_handler = None;
        ctx.save_session_state(&state)?;
    }

    if let Some(block_patterns) = state.block_patterns {
        let mut blocked = block_patterns.url_patterns;
        for rt in block_patterns.resource_types {
            blocked.extend(resource_type_url_patterns(&rt));
        }
        if !blocked.is_empty() {
            cdp.send("Network.enable", json!({})).await?;
            let uniq = blocked
                .into_iter()
                .collect::<HashSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            cdp.send("Network.setBlockedURLs", json!({ "urls": uniq }))
                .await?;
        }
    }

    Ok(())
}

pub async fn get_page_brief(cdp: &mut CdpClient) -> Result<String> {
    let result = runtime_evaluate(cdp, PAGE_BRIEF_SCRIPT, true, false).await?;
    Ok(result
        .pointer("/result/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

pub async fn wait_for_ready_state_complete(cdp: &mut CdpClient, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() <= deadline {
        let result = runtime_evaluate(cdp, "document.readyState", true, false).await?;
        let state = result
            .pointer("/result/value")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if state == "complete" {
            return Ok(());
        }
        sleep(Duration::from_millis(300)).await;
    }
    Ok(())
}

/// Wait until no network requests are in-flight for `quiet_ms`.
/// Gives up after `timeout_ms` total and proceeds anyway.
pub async fn wait_for_network_idle(
    cdp: &mut CdpClient,
    quiet_ms: u64,
    timeout_ms: u64,
) -> Result<()> {
    cdp.send("Network.enable", json!({})).await?;

    let mut inflight: i32 = 0;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let quiet = Duration::from_millis(quiet_ms);
    let mut last_activity = Instant::now();

    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        if inflight <= 0 && now.duration_since(last_activity) >= quiet {
            break;
        }
        // Cap wait to quiet period so we re-check idle condition promptly
        let remain = std::cmp::min(deadline.saturating_duration_since(now), quiet);
        let Some(event) = cdp.next_event(remain).await? else {
            // Timeout — re-check idle condition at top of loop
            continue;
        };
        if event.is_null() {
            continue;
        }
        match event
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "Network.requestWillBeSent" => {
                inflight += 1;
                last_activity = Instant::now();
            }
            "Network.loadingFinished" | "Network.loadingFailed" => {
                inflight -= 1;
                last_activity = Instant::now();
            }
            _ => {}
        }
    }

    cdp.send("Network.disable", json!({})).await?;
    Ok(())
}

pub async fn locate_element(
    ctx: &AppContext,
    cdp: &mut CdpClient,
    selector: &str,
) -> Result<LocatedElement> {
    let context_id = get_frame_context_id(ctx, cdp).await?;
    let script = format!(
        r#"
      (async function() {{
        const sel = {sel};
        let el;
        try {{
          for (let i = 0; i < 50; i++) {{
            el = document.querySelector(sel);
            if (el) break;
            await new Promise(r => setTimeout(r, 100));
          }}
        }} catch (e) {{
          return {{ error: 'Invalid CSS selector: ' + sel + '. Use CSS selectors (#id, .class, tag).' }};
        }}
        if (!el) return {{ error: 'Element not found after 5s: ' + sel }};
        el.scrollIntoView({{ block: 'center', inline: 'center', behavior: 'instant' }});
        await new Promise(r => setTimeout(r, 50));
        const rect = el.getBoundingClientRect();
        return {{
          x: rect.left + rect.width / 2,
          y: rect.top + rect.height / 2,
          tag: el.tagName,
          text: (el.textContent || '').substring(0, 50).trim()
        }};
      }})()
    "#,
        sel = serde_json::to_string(selector)?
    );

    let result = runtime_evaluate_with_context(cdp, &script, true, true, context_id).await?;
    let value = result
        .pointer("/result/value")
        .cloned()
        .unwrap_or(Value::Null);

    if let Some(err) = value.get("error").and_then(Value::as_str) {
        bail!("{err}");
    }

    let x = value
        .get("x")
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("Element location missing x"))?;
    let y = value
        .get("y")
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("Element location missing y"))?;
    let tag = value
        .get("tag")
        .and_then(Value::as_str)
        .unwrap_or("element")
        .to_string();
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    Ok(LocatedElement { x, y, tag, text })
}

pub async fn locate_element_by_text(
    ctx: &AppContext,
    cdp: &mut CdpClient,
    text: &str,
) -> Result<LocatedElement> {
    let context_id = get_frame_context_id(ctx, cdp).await?;
    let script = format!(
        r#"
      (function() {{
        const target = {target};
        const lower = target.toLowerCase();
        let best = null;
        let bestLen = Infinity;

        function* allElements(root) {{
          for (const el of root.querySelectorAll('*')) {{
            yield el;
            if (el.shadowRoot) yield* allElements(el.shadowRoot);
          }}
        }}

        function isInteractive(el) {{
          if (!el) return false;
          return ['A','BUTTON','INPUT','SELECT','TEXTAREA','SUMMARY'].includes(el.tagName)
            || el.getAttribute('role') === 'button'
            || el.getAttribute('role') === 'link'
            || el.getAttribute('role') === 'menuitem'
            || el.getAttribute('role') === 'tab';
        }}

        function actionableAncestor(el) {{
          let cur = el;
          for (let depth = 0; cur && depth < 5; depth += 1) {{
            if (isInteractive(cur)) return cur;
            const parent = cur.parentNode;
            if (parent instanceof ShadowRoot) {{
              cur = parent.host;
            }} else {{
              cur = cur.parentElement;
            }}
          }}
          return el;
        }}

        for (const el of allElements(document)) {{
          if (el.offsetParent === null && el.tagName !== 'BODY' && el.tagName !== 'HTML') {{
            const s = getComputedStyle(el);
            if (s.display === 'none' || (s.position !== 'fixed' && s.position !== 'sticky')) continue;
          }}
          const t = (el.textContent || '').trim();
          if (!t) continue;
          const tl = t.toLowerCase();
          const exact = tl === lower;
          const has = tl.includes(lower);
          if (!exact && !has) continue;
          const clickEl = isInteractive(el) ? el : actionableAncestor(el);
          const interactive = isInteractive(clickEl);
          const len = t.length;
          if (exact) {{
            if (!best || !best.exact || (interactive && !best.interactive) || (interactive === best.interactive && len < bestLen)) {{
              best = {{ el: clickEl, exact: true, interactive, matchedText: t }}; bestLen = len;
            }}
          }} else if (has && !(best && best.exact)) {{
            if (!best || (interactive && !best.interactive) || (interactive === best.interactive && len < bestLen)) {{
              best = {{ el: clickEl, exact: false, interactive, matchedText: t }}; bestLen = len;
            }}
          }}
        }}

        if (!best) return {{ error: 'No visible element with text: ' + target }};
        const el = best.el;
        el.scrollIntoView({{ block: 'center', inline: 'center', behavior: 'instant' }});
        const rect = el.getBoundingClientRect();
        return {{
          x: rect.left + rect.width / 2,
          y: rect.top + rect.height / 2,
          tag: el.tagName,
          text: (best.matchedText || el.textContent || '').substring(0, 50).trim()
        }};
      }})()
    "#,
        target = serde_json::to_string(text)?
    );

    let result = runtime_evaluate_with_context(cdp, &script, true, false, context_id).await?;
    let value = result
        .pointer("/result/value")
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(err) = value.get("error").and_then(Value::as_str) {
        bail!("{err}");
    }
    let x = value
        .get("x")
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("Element location missing x"))?;
    let y = value
        .get("y")
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("Element location missing y"))?;
    let tag = value
        .get("tag")
        .and_then(Value::as_str)
        .unwrap_or("element")
        .to_string();
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(LocatedElement { x, y, tag, text })
}

pub async fn snapshot_tab_ids(ctx: &AppContext) -> Result<HashSet<String>> {
    Ok(get_debug_tabs(ctx)
        .await?
        .into_iter()
        .map(|tab| tab.id)
        .collect())
}

pub async fn adopt_new_tabs(
    ctx: &mut AppContext,
    before: &HashSet<String>,
    timeout: Duration,
) -> Result<Vec<DebugTab>> {
    let expected_window = ctx.load_session_state()?.window_id;
    let deadline = Instant::now() + timeout;

    loop {
        let tabs = get_debug_tabs(ctx).await?;
        let mut new_tabs = tabs
            .into_iter()
            .filter(|tab| !before.contains(&tab.id))
            .collect::<Vec<_>>();

        if let Some(window_id) = expected_window {
            let mut same_window = Vec::new();
            for tab in new_tabs {
                let Some(ws_url) = tab.web_socket_debugger_url.as_deref() else {
                    continue;
                };
                if get_window_id_for_target(ctx, ws_url).await.ok() == Some(window_id) {
                    same_window.push(tab);
                }
            }
            new_tabs = same_window;
        } else if new_tabs.len() > 1 {
            new_tabs.clear();
        }

        if !new_tabs.is_empty() {
            let mut state = ctx.load_session_state()?;
            let max_tabs = crate::config::load_config().max_tabs;
            if state.tabs.len() >= max_tabs {
                wlog!(
                    "tab limit ({max_tabs}) reached during adoption — consider closing unused tabs"
                );
            }
            for tab in &new_tabs {
                if !state.tabs.iter().any(|id| id == &tab.id) {
                    state.tabs.push(tab.id.clone());
                }
            }

            let active = new_tabs
                .iter()
                .find(|tab| tab.url.as_deref().is_some_and(|url| url != "about:blank"))
                .or_else(|| new_tabs.first())
                .map(|tab| tab.id.clone());

            if let Some(active_tab_id) = active {
                state.active_tab_id = Some(active_tab_id);
            }
            ctx.save_session_state(&state)?;
            return Ok(new_tabs);
        }

        if Instant::now() >= deadline {
            return Ok(Vec::new());
        }
        sleep(Duration::from_millis(100)).await;
    }
}

pub async fn focus_editable_element(
    cdp: &mut CdpClient,
    context_id: Option<i64>,
    selector: &str,
    select_existing: bool,
) -> Result<()> {
    let query = deep_query_expr(selector)?;
    let script = format!(
        r#"(function() {{
          const found = {query};
          if (found && found.error) return found;
          const el = found;
          if (!el) return {{ error: 'Element not found: ' + {sel} }};
          el.focus();
          if ({select_existing} && typeof el.select === 'function' && el.type !== 'password') {{
            el.select();
          }}
          return {{ ok: true }};
        }})()"#,
        query = query,
        sel = serde_json::to_string(selector)?,
        select_existing = if select_existing { "true" } else { "false" }
    );
    let result = runtime_evaluate_with_context(cdp, &script, true, false, context_id).await?;
    check_js_error(&result)?;
    Ok(())
}

pub async fn clear_editable_element(
    cdp: &mut CdpClient,
    context_id: Option<i64>,
    selector: &str,
) -> Result<()> {
    let query = deep_query_expr(selector)?;
    let script = format!(
        r#"(function() {{
          const found = {query};
          if (found && found.error) return found;
          const el = found;
          if (!el) return {{ error: 'Element not found: ' + {sel} }};
          el.focus();
          if ('value' in el) {{
            el.value = '';
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
          }} else if (el.isContentEditable) {{
            el.textContent = '';
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
          }}
          return {{ ok: true }};
        }})()"#,
        query = query,
        sel = serde_json::to_string(selector)?
    );
    let result = runtime_evaluate_with_context(cdp, &script, true, false, context_id).await?;
    check_js_error(&result)?;
    Ok(())
}

pub async fn editable_element_value(
    cdp: &mut CdpClient,
    context_id: Option<i64>,
    selector: &str,
) -> Result<String> {
    let query = deep_query_expr(selector)?;
    let script = format!(
        r#"(function() {{
          const found = {query};
          if (found && found.error) return found;
          const el = found;
          if (!el) return {{ error: 'Element not found: ' + {sel} }};
          const value = 'value' in el
            ? String(el.value || '')
            : (el.isContentEditable ? String(el.textContent || '') : String(el.textContent || ''));
          return {{ value }};
        }})()"#,
        query = query,
        sel = serde_json::to_string(selector)?
    );
    let result = runtime_evaluate_with_context(cdp, &script, true, false, context_id).await?;
    check_js_error(&result)?;
    Ok(result
        .pointer("/result/value/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

async fn type_text_via_key_events(cdp: &mut CdpClient, text: &str) -> Result<()> {
    for ch in text.chars() {
        let char_s = ch.to_string();
        cdp.send(
            "Input.dispatchKeyEvent",
            json!({ "type": "keyDown", "text": char_s, "unmodifiedText": char_s }),
        )
        .await?;
        cdp.send(
            "Input.dispatchKeyEvent",
            json!({ "type": "keyUp", "text": ch.to_string(), "unmodifiedText": ch.to_string() }),
        )
        .await?;
        sleep(Duration::from_millis(12)).await;
    }
    Ok(())
}

pub async fn type_text_verified(
    cdp: &mut CdpClient,
    context_id: Option<i64>,
    selector: &str,
    text: &str,
) -> Result<()> {
    focus_editable_element(cdp, context_id, selector, true).await?;
    type_text_via_key_events(cdp, text).await?;
    if editable_element_value(cdp, context_id, selector).await? == text {
        return Ok(());
    }

    clear_editable_element(cdp, context_id, selector).await?;
    focus_editable_element(cdp, context_id, selector, false).await?;
    cdp.send("Input.insertText", json!({ "text": text }))
        .await?;
    sleep(Duration::from_millis(50)).await;
    if editable_element_value(cdp, context_id, selector).await? == text {
        return Ok(());
    }

    let query = deep_query_expr(selector)?;
    let set_script = format!(
        r#"(function() {{
          const found = {query};
          if (found && found.error) return found;
          const el = found;
          if (!el) return {{ error: 'Element not found: ' + {sel} }};
          if ('value' in el) {{
            const proto = el.tagName === 'TEXTAREA'
              ? HTMLTextAreaElement.prototype
              : HTMLInputElement.prototype;
            const setter = Object.getOwnPropertyDescriptor(proto, 'value')?.set;
            if (setter) setter.call(el, {text});
            else el.value = {text};
            el.dispatchEvent(new InputEvent('input', {{
              bubbles: true,
              inputType: 'insertText',
              data: {text}
            }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
          }} else if (el.isContentEditable) {{
            el.textContent = {text};
            el.dispatchEvent(new InputEvent('input', {{
              bubbles: true,
              inputType: 'insertText',
              data: {text}
            }}));
          }} else {{
            return {{ error: 'Element is not editable: ' + {sel} }};
          }}
          return {{ ok: true }};
        }})()"#,
        query = query,
        sel = serde_json::to_string(selector)?,
        text = serde_json::to_string(text)?
    );
    let result = runtime_evaluate_with_context(cdp, &set_script, true, false, context_id).await?;
    check_js_error(&result)?;

    if editable_element_value(cdp, context_id, selector).await? == text {
        return Ok(());
    }

    bail!("Typed text did not stick in {selector}");
}

pub fn resolve_selector(ctx: &AppContext, input: &str) -> Result<String> {
    if input.chars().all(|c| c.is_ascii_digit()) {
        let state = ctx.load_session_state()?;
        let map = state
            .ref_map
            .ok_or_else(|| anyhow!("No ref map. Run: ax-tree -i"))?;
        let selector = map
            .get(input)
            .cloned()
            .ok_or_else(|| anyhow!("Ref {input} not found. Run: ax-tree -i to refresh."))?;
        return Ok(selector);
    }
    Ok(input.to_string())
}

#[derive(Debug)]
pub struct InteractiveData {
    pub elements: Vec<InteractiveElement>,
    pub output: String,
}

pub async fn fetch_interactive_elements(
    ctx: &mut AppContext,
    cdp: &mut CdpClient,
) -> Result<InteractiveData> {
    let current_url_result = runtime_evaluate(cdp, "location.href", true, false).await?;
    let current_url = current_url_result
        .pointer("/result/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let cache_key = cache_key_from_url(&current_url);

    let mut action_cache = load_action_cache(ctx)?;
    if let Some(cached) = action_cache.get(&cache_key).cloned() {
        if now_epoch_ms() - cached.timestamp < CACHE_TTL_MS && !cached.ref_map.is_empty() {
            let refs_to_check = cached.ref_map.values().take(3).cloned().collect::<Vec<_>>();
            let mut valid = !refs_to_check.is_empty();
            for sel in refs_to_check {
                let check = runtime_evaluate(
                    cdp,
                    &format!("!!document.querySelector({})", serde_json::to_string(&sel)?),
                    true,
                    false,
                )
                .await?;
                if !check
                    .pointer("/result/value")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    valid = false;
                    break;
                }
            }
            // Also check if overlays/modals appeared since cache was built
            if valid {
                let overlay_check = runtime_evaluate(
                    cdp,
                    "document.querySelectorAll('[role=dialog],[role=alertdialog],[role=menu],[role=listbox],[aria-modal=true],[aria-modal=\"true\"],.modal,.modal-dialog,.drawer,.popover,[data-modal],[data-state=open],[data-headlessui-state~=open]').length",
                    true,
                    false,
                )
                .await?;
                let overlay_count = overlay_check
                    .pointer("/result/value")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                if overlay_count > 0 {
                    valid = false; // Force re-scan when overlays are present
                }
            }
            if valid {
                let mut state = ctx.load_session_state()?;
                state.prev_elements = state.current_elements.clone();
                state.current_elements = Some(cached.elements.clone());
                state.ref_map = Some(cached.ref_map.clone());
                state.ref_map_url = Some(current_url);
                state.ref_map_timestamp = Some(cached.timestamp);
                ctx.save_session_state(&state)?;
                return Ok(InteractiveData {
                    elements: cached.elements,
                    output: cached.output,
                });
            }
        }
    }

    let script = AXTREE_INTERACTIVE_SCRIPT.replace("__SIDEKAR_SELECTOR_GEN__", SELECTOR_GEN_SCRIPT);
    let context_id = get_frame_context_id(ctx, cdp).await?;
    let result = runtime_evaluate_with_context(cdp, &script, true, false, context_id).await?;
    let items = result
        .pointer("/result/value")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut elements = Vec::new();
    let mut ref_map = HashMap::new();
    let mut lines = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        let ref_id = idx + 1;
        let selector = item
            .get("selector")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let role = item
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("element")
            .to_string();
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let value = item
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        lines.push(if name.is_empty() {
            format!("[{}] {}", ref_id, role)
        } else {
            format!("[{}] {} \"{}\"", ref_id, role, truncate(&name, 80))
        });
        ref_map.insert(ref_id.to_string(), selector);
        elements.push(InteractiveElement {
            ref_id,
            role,
            name,
            value,
        });
    }
    let mut output = lines.join("\n");
    if output.len() > 6000 {
        let boundary = output.floor_char_boundary(6000);
        output = format!("{}\n... (truncated)", &output[..boundary]);
    }
    if output.is_empty() {
        output = "(no interactive elements found)".to_string();
    }

    let mut state = ctx.load_session_state()?;
    state.prev_elements = state.current_elements.clone();
    state.current_elements = Some(elements.clone());
    state.ref_map = Some(ref_map.clone());
    state.ref_map_url = Some(current_url.clone());
    state.ref_map_timestamp = Some(now_epoch_ms());
    ctx.save_session_state(&state)?;

    action_cache.insert(
        cache_key,
        ActionCacheEntry {
            ref_map: ref_map.clone(),
            elements: elements.clone(),
            output: output.clone(),
            timestamp: now_epoch_ms(),
        },
    );
    save_action_cache(ctx, &action_cache)?;

    Ok(InteractiveData { elements, output })
}

pub fn diff_elements(
    prev: &[InteractiveElement],
    curr: &[InteractiveElement],
) -> (
    Vec<InteractiveElement>,
    Vec<InteractiveElement>,
    Vec<(InteractiveElement, InteractiveElement)>,
) {
    let prev_map = prev
        .iter()
        .map(|e| (e.ref_id, e.clone()))
        .collect::<HashMap<_, _>>();
    let curr_map = curr
        .iter()
        .map(|e| (e.ref_id, e.clone()))
        .collect::<HashMap<_, _>>();

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();

    for (ref_id, el) in &curr_map {
        if let Some(old) = prev_map.get(ref_id) {
            if old.role != el.role || old.name != el.name || old.value != el.value {
                changed.push((old.clone(), el.clone()));
            }
        } else {
            added.push(el.clone());
        }
    }
    for (ref_id, el) in &prev_map {
        if !curr_map.contains_key(ref_id) {
            removed.push(el.clone());
        }
    }
    (added, removed, changed)
}

pub fn cache_key_from_url(url: &str) -> String {
    if let Ok(parsed) = reqwest::Url::parse(url) {
        format!("{}{}", parsed.host_str().unwrap_or_default(), parsed.path())
    } else {
        url.to_string()
    }
}

pub fn load_action_cache(ctx: &AppContext) -> Result<HashMap<String, ActionCacheEntry>> {
    let path = ctx.action_cache_file();
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed reading {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("failed parsing {}", path.display()))
}

pub fn save_action_cache(
    ctx: &AppContext,
    cache: &HashMap<String, ActionCacheEntry>,
) -> Result<()> {
    let now = now_epoch_ms();
    let mut entries = cache
        .iter()
        .filter(|(_, v)| now - v.timestamp <= CACHE_TTL_MS)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| b.1.timestamp.cmp(&a.1.timestamp));
    entries.truncate(CACHE_MAX_ENTRIES);
    let pruned = entries.into_iter().collect::<HashMap<_, _>>();
    let path = ctx.action_cache_file();
    atomic_write_json(&path, &pruned)
}

/// Read-modify-write tab locks under an exclusive file lock.
/// Uses a separate `.lock` file to avoid flock+rename inode mismatch.
pub(crate) fn with_tab_locks_exclusive<F, R>(ctx: &AppContext, f: F) -> Result<R>
where
    F: FnOnce(&mut HashMap<String, TabLock>) -> Result<R>,
{
    let path = ctx.tab_locks_file();
    let lock_path = path.with_extension("lock");
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed opening lock file {}", lock_path.display()))?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("failed locking {}", lock_path.display()))?;
    let mut locks: HashMap<String, TabLock> = if path.exists() {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed reading {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("failed parsing {}", path.display()))?
    } else {
        HashMap::new()
    };
    let result = f(&mut locks)?;
    atomic_write_json(&path, &locks)?;
    Ok(result)
}

pub fn check_tab_lock(ctx: &AppContext, tab_id: &str) -> Result<Option<TabLock>> {
    let tab_id = tab_id.to_string();
    let now = now_epoch_ms();
    with_tab_locks_exclusive(ctx, |locks| {
        if let Some(lock) = locks.get(&tab_id).cloned() {
            // Use saturating_sub to handle clock skew (if clock goes backwards, lock stays valid)
            if now.saturating_sub(lock.expires) > 0 {
                locks.remove(&tab_id);
                return Ok(None);
            }
            return Ok(Some(lock));
        }
        Ok(None)
    })
}

pub fn print_command_help(command: &str) {
    if let Some(replacement) = removed_command_replacement(command) {
        println!("Command '{command}' was removed.\n\nUse: sidekar {replacement}");
        return;
    }

    let command = canonical_command_name(command).unwrap_or(command);
    let help = match command {
        "navigate" => {
            "\
sidekar navigate <url> [--no-dismiss]

  Navigate the active tab to <url>. Automatically adds https:// if no scheme.
  Auto-dismisses cookie consent banners and common popups after load.
  Returns a page brief with URL, title, visible inputs, buttons, links.

  Options:
    --no-dismiss   Skip automatic popup/banner dismissal

  Examples:
    sidekar navigate example.com
    sidekar navigate https://github.com/search?q=rust --no-dismiss"
        }

        "click" => {
            "\
sidekar click <target> [--mode=double|right|human]

  Click an element. Waits up to 5s for it to appear, scrolls into view.

  Target types (in priority order):
    <ref>          Ref number from ax-tree -i, observe, or text (e.g. 3)
    --text <text>  Find by visible text, prefer interactive ancestors
    <selector>     CSS selector (#id, .class, [data-testid=...])
    <x>,<y>        Coordinates from screenshot (last resort)

  Modes:
    --mode=double  Double-click
    --mode=right   Right-click / context menu
    --mode=human   Bezier curve mouse movement for bot detection evasion

  On macOS, --text auto-falls back to Accessibility API for Chrome-native UI
  (permission dialogs, extension popups) if not found in DOM.

  Examples:
    sidekar click 3
    sidekar click --text \"Sign in\"
    sidekar click \"#submit-btn\"
    sidekar click --mode=double 5
    sidekar click 450,300"
        }

        "type" => {
            "\
sidekar type <selector> <text> [--human]

  Focus the element matching <selector> and type <text> into it.
  Clears existing content first.

  Options:
    --human   Human-like typing with variable delays and occasional typos

  Use 'keyboard' instead for rich text editors where focus resets cursor.

  Examples:
    sidekar type \"#search\" \"rust async\"
    sidekar type 5 \"hello world\"
    sidekar type --human \"#email\" \"user@example.com\""
        }

        "keyboard" => {
            "\
sidekar keyboard <text>

  Type text at the current caret position without focusing a new element.
  Essential for rich text editors (Slack, Google Docs, Notion) where
  'type' would reset the cursor position.

  Example:
    sidekar click \".editor\"
    sidekar keyboard \"Hello world\""
        }

        "fill" => {
            "\
sidekar fill <selector1> <value1> [selector2] [value2] ...

  Fill multiple form fields in one call. Alternating selector/value pairs.
  More efficient than multiple 'type' calls.

  Examples:
    sidekar fill \"#email\" \"user@example.com\" \"#password\" \"secret\"
    sidekar fill 3 \"Alice\" 5 \"alice@example.com\""
        }

        "read" => {
            "\
sidekar read [selector] [--tokens=N]

  Reader-mode text extraction. Strips navigation, sidebars, ads.
  Returns clean text with headings, lists, paragraphs.
  Best for articles, documentation, search results.

  Options:
    selector     CSS selector to scope extraction
    --tokens=N   Approximate token limit for output

  Examples:
    sidekar read
    sidekar read article --tokens=2000
    sidekar read \".main-content\""
        }

        "text" => {
            "\
sidekar text [selector] [--tokens=N]

  Full page text in reading order, interleaving static text with
  interactive elements (numbered refs). Like a screen reader view.
  Generates ref map as side effect.

  Best for complex pages where you need both content and interaction targets.

  Examples:
    sidekar text
    sidekar text --tokens=3000"
        }

        "ax-tree" => {
            "\
sidekar ax-tree [options] [selector]

  Accessibility tree — semantic roles and accessible names.

  Options:
    -i, --interactive   Show only actionable elements with ref numbers (flat list)
    --diff              Show only changes since last snapshot
    --tokens=N          Approximate token limit

  After -i, use ref numbers everywhere: click 3, type 5 \"hello\", screenshot --ref=7

  Examples:
    sidekar ax-tree -i
    sidekar ax-tree -i --diff
    sidekar ax-tree --tokens=2000"
        }

        "dom" => {
            "\
sidekar dom [selector] [--tokens=N]

  Compact DOM tree with scripts, styles, SVGs stripped.
  Traverses open shadow roots. Scope with CSS selector.

  Examples:
    sidekar dom
    sidekar dom \"main\" --tokens=3000
    sidekar dom \"#app\""
        }

        "screenshot" => {
            "\
sidekar screenshot [options]

  Capture a screenshot of the page or a specific element.

  Options:
    --ref=N            Crop to ref number (from ax-tree -i, observe, text)
    --selector=SEL     Crop to CSS selector
    --full             Capture entire scrollable page
    --output=PATH      Save to specific file path
    --format=FMT       png or jpeg (default: jpeg)
    --quality=N        JPEG quality 1-100
    --scale=N          Scale factor (default: fit 800px width)
    --pad=N            Padding around crop in pixels (default: 48)

  Examples:
    sidekar screenshot
    sidekar screenshot --ref=3
    sidekar screenshot --selector=\".modal\" --format=png
    sidekar screenshot --full --output=/tmp/page.png"
        }

        "press" => {
            "\
sidekar press <key>

  Press a key or key combination.

  Common keys: Enter, Tab, Escape, Backspace, ArrowUp, ArrowDown, Space
  Modifiers: Ctrl+A, Meta+C, Meta+V, Shift+Enter, Alt+Tab
  Mac note: Use Meta (not Ctrl) for app shortcuts. Meta+Alt+2 for Heading 2.

  Examples:
    sidekar press Enter
    sidekar press Ctrl+A
    sidekar press Meta+V
    sidekar press Shift+Enter"
        }

        "scroll" => {
            "\
sidekar scroll <target> [pixels]

  Scroll the page or a specific container.

  Targets:
    up / down       Scroll page (default 400px)
    top / bottom    Scroll to page extremes
    <selector>      Scroll element into view
    <selector> up   Scroll within a container

  Examples:
    sidekar scroll down
    sidekar scroll down 800
    sidekar scroll top
    sidekar scroll \".chat-messages\" down"
        }

        "search" => {
            "\
sidekar search <query> [--engine=E] [--tokens=N]

  Web search via real browser. Navigates to search engine, submits query,
  extracts results with 'read'. Returns formatted results.

  Engines: google (default), bing, duckduckgo, or a custom URL (query appended)

  Examples:
    sidekar search \"rust async programming\"
    sidekar search --engine=bing \"weather forecast\""
        }

        "read-urls" => {
            "\
sidekar read-urls <url1> <url2> ... [--tokens=N]

  Read multiple URLs in parallel. Opens each in a new tab,
  extracts content, returns combined results, closes tabs.

  Examples:
    sidekar read-urls https://example.com https://example.org"
        }

        "batch" => {
            "\
sidekar batch '<json>'

  Execute multiple actions sequentially in one call.

  JSON format: {\"actions\": [...], \"delay\": 0}
  Each action: {\"tool\": \"<cmd>\", ...params, \"wait\": ms, \"retries\": N, \"optional\": bool}
  Smart waits: 500ms auto-added after state-changing actions.

  Example:
    sidekar batch '{\"actions\":[
      {\"tool\":\"click\",\"target\":\"--text Continue\",\"retries\":2},
      {\"tool\":\"wait-for-nav\"},
      {\"tool\":\"screenshot\",\"output\":\"/tmp/result.png\"}
    ]}'"
        }

        "launch" => {
            "\
sidekar launch [options]

  Launch a Chromium browser and create a session.

  Options:
    --browser=NAME   chrome, edge, brave, arc, vivaldi, chromium, canary
    --profile=NAME   Named profile for isolated browser data ('new' for auto-ID)
    --headless       No visible window (all tools still work)

  Examples:
    sidekar launch
    sidekar launch --browser=brave --profile=testing
    sidekar launch --headless"
        }

        "connect" => {
            "\
sidekar connect

  Attach to an already-running browser debug port and create a new Sidekar session.
  Does not launch a new browser process.

  Example:
    sidekar connect"
        }

        "desktop" => {
            "\
sidekar desktop <subcommand> [args...]

  Desktop automation via the macOS Accessibility API.

  Subcommands:
    screenshot [--app <name>|--pid <pid>] [--output <path>]
    apps
    windows --app <name>|--pid <pid>
    find --app <name>|--pid <pid> <query>
    click --app <name>|--pid <pid> <query>
    press <key|combo>
    type <text>
    paste <text>
    launch <app>
    activate --app <name>|--pid <pid>
    quit --app <name>|--pid <pid>

  Examples:
    sidekar desktop apps
    sidekar desktop screenshot --app Safari
    sidekar desktop click --app Finder \"New Folder\""
        }

        "tabs" => "sidekar tabs\n\n  List all tabs owned by this session.",
        "tab" => "sidekar tab <id>\n\n  Switch to a tab by ID (from 'tabs' output).",
        "new-tab" => "sidekar new-tab [url]\n\n  Open a new tab, optionally navigating to URL.",
        "close" => "sidekar close\n\n  Close the current tab and switch to the next.",
        "back" => "sidekar back\n\n  Go back in browser history.",
        "forward" => "sidekar forward\n\n  Go forward in browser history.",
        "reload" => "sidekar reload\n\n  Reload the current page.",
        "observe" => {
            "sidekar observe\n\n  Show interactive elements formatted as ready-to-use commands.\n  Generates ref map. Like 'ax-tree -i' but with command suggestions."
        }
        "find" => {
            "sidekar find <query>\n\n  Find an element by natural language description.\n\n  Example: sidekar find \"the login button\""
        }
        "resolve" => {
            "sidekar resolve <selector>\n\n  Get link/form target URL without clicking.\n  Returns href, action, formAction, src, onclick, target attributes.\n\n  Example: sidekar resolve 3"
        }

        "eval" => {
            "\
sidekar eval <javascript>

  Evaluate a JavaScript expression in the page context.
  Returns the result.

  Examples:
    sidekar eval \"document.title\"
    sidekar eval \"document.querySelectorAll('a').length\"
    sidekar eval \"document.querySelector('#btn').click()\""
        }

        "cookies" => {
            "\
sidekar cookies [action] [name] [value] [domain]

  Actions: get (default), set, delete, clear

  Examples:
    sidekar cookies
    sidekar cookies set session abc123
    sidekar cookies delete tracking
    sidekar cookies clear"
        }

        "console" => {
            "\
sidekar console [action]

  Actions:
    show (default)   Display current console messages
    listen           Stream console events (long-running)

  Examples:
    sidekar console
    sidekar console show
    sidekar console listen"
        }

        "network" => {
            "\
sidekar network [action] [duration] [filter]

  Actions:
    capture [secs] [filter]   Record XHR/fetch requests (default 10s)
    show                      Re-display last capture

  Examples:
    sidekar network capture 15
    sidekar network capture 10 api/users
    sidekar network show"
        }

        "block" => {
            "\
sidekar block <patterns...>

  Block resource types or URL patterns. Use 'off' to disable all.
  Resource types: images, css, fonts, media, scripts

  Examples:
    sidekar block images fonts
    sidekar block analytics.js
    sidekar block off"
        }

        "viewport" => {
            "\
sidekar viewport <preset|width> [height]

  Presets: mobile (375x667), iphone (390x844), ipad (820x1180),
           tablet (768x1024), desktop (1280x800)
  Or exact: sidekar viewport 1920 1080

  Examples:
    sidekar viewport mobile
    sidekar viewport 1440 900"
        }

        "zoom" => {
            "\
sidekar zoom <level>

  Zoom: in (+25%), out (-25%), reset (100%), or exact number (25-200).
  Coordinate clicks auto-adjust. Use 'zoom out' before full-page screenshots.

  Examples:
    sidekar zoom out
    sidekar zoom 50
    sidekar zoom reset"
        }

        "dialog" => {
            "\
sidekar dialog <accept|dismiss> [prompt_text]

  Set a one-shot handler for the next JavaScript dialog (alert/confirm/prompt).
  Must be called BEFORE the action that triggers the dialog.

  Examples:
    sidekar dialog accept
    sidekar dialog dismiss
    sidekar dialog accept \"my input text\""
        }

        "wait-for" => {
            "\
sidekar wait-for <selector> [timeout_ms]

  Wait for an element to appear in the DOM (default timeout: 30s).

  Examples:
    sidekar wait-for \".results\"
    sidekar wait-for \"#modal\" 5000"
        }

        "wait-for-nav" => {
            "\
sidekar wait-for-nav [timeout_ms]

  Wait for navigation to complete (document.readyState === 'complete').
  Default timeout: 10s.

  Example:
    sidekar wait-for-nav
    sidekar wait-for-nav 15000"
        }

        "select" => {
            "sidekar select <selector> <value> [value2...]\n\n  Select option(s) from a <select> element by value or label.\n\n  Example: sidekar select \"#country\" \"US\""
        }
        "upload" => {
            "sidekar upload <selector> <file> [file2...]\n\n  Upload file(s) to a file input element.\n\n  Example: sidekar upload \"input[type=file]\" /tmp/photo.jpg"
        }
        "drag" => {
            "sidekar drag <from> <to>\n\n  Drag from one element to another.\n\n  Example: sidekar drag \"#item-1\" \"#drop-zone\""
        }
        "paste" => {
            "sidekar paste <text>\n\n  Paste text via ClipboardEvent. Works with apps that intercept paste."
        }
        "clipboard" => {
            "\
sidekar clipboard --html <html> [--text <text>]

  Write HTML to clipboard and paste via Cmd+V.
  Works with Google Docs, Sheets, Notion — apps that ignore synthetic paste.

  Examples:
    sidekar clipboard --html \"<b>bold</b> text\"
    sidekar clipboard --html \"<h1>Title</h1>\" --text \"Title\""
        }

        "insert-text" => {
            "sidekar insert-text <text>\n\n  Insert text at cursor via CDP Input.insertText.\n  Faster than keyboard for large text. No formatting — use clipboard for rich text."
        }
        "hover" => {
            "sidekar hover <target>\n\n  Hover over an element (same targeting as click: ref, --text, selector, x,y)."
        }
        "focus" => "sidekar focus <selector>\n\n  Focus an element without clicking it.",
        "clear" => "sidekar clear <selector>\n\n  Clear an input or contenteditable element.",

        "storage" => {
            "\
sidekar storage <action> [key] [value] [--session]

  Actions: get, set, remove, clear
  For 'clear': target can be 'everything' (storage + cache + cookies + SW)

  Options:
    --session   Operate on sessionStorage instead of localStorage

  Examples:
    sidekar storage get
    sidekar storage set mykey myvalue
    sidekar storage clear everything"
        }

        "service-workers" => {
            "\
sidekar service-workers <action>

  Actions: list, unregister, update
  Manage service workers for the current page origin.

  Examples:
    sidekar service-workers list
    sidekar service-workers unregister"
        }

        "security" => {
            "\
sidekar security <action>

  Actions:
    ignore-certs   Accept self-signed/invalid certificates
    strict         Restore normal certificate validation

  Example: sidekar security ignore-certs"
        }

        "media" => {
            "\
sidekar media <features...>

  Emulate media features. Use 'reset' to restore defaults.

  Features: dark, light, print, reduce-motion, etc.

  Examples:
    sidekar media dark
    sidekar media print
    sidekar media reset"
        }

        "animations" => {
            "sidekar animations <pause|resume|slow>\n\n  pause: freeze all animations\n  resume: restore normal playback\n  slow: 10% speed"
        }
        "grid" => {
            "\
sidekar grid [spec]

  Overlay a coordinate grid for canvas/image targeting.

  Specs: 8x6 (cols x rows), 50 (pixel cell size), off (remove)
  Default: 10x10 grid. Take a screenshot after to see coordinates.

  Example: sidekar grid 8x6"
        }

        "pdf" => "sidekar pdf [path]\n\n  Save current page as PDF. Default: temp directory.",
        "download" => {
            "sidekar download [action] [path]\n\n  Actions: path (set download dir), list (show downloads)\n\n  Example: sidekar download path /tmp/downloads"
        }
        "frames" => "sidekar frames\n\n  List all frames/iframes in the page.",
        "frame" => {
            "sidekar frame <target>\n\n  Switch to a frame by ID, name, or CSS selector.\n  Use 'main' to switch back to the top frame.\n\n  Example: sidekar frame \"iframe.content\""
        }
        "lock" => {
            "sidekar lock [seconds]\n\n  Lock the active tab for exclusive access (default: 300s)."
        }
        "unlock" => "sidekar unlock\n\n  Release the tab lock.",
        "activate" => "sidekar activate\n\n  Bring the browser window to the front (macOS).",
        "minimize" => "sidekar minimize\n\n  Minimize the browser window (macOS).",
        "kill" => "sidekar kill\n\n  Kill the custom profile browser session.",

        "bus" => {
            "\
sidekar bus <who|requests|replies|show|send|done> [args...]

  Agent bus subcommands:
    who
    requests [--status=open|answered|timed-out|cancelled|all] [--limit=N]
    replies [--msg-id=<request_id>] [--limit=N]
    show <msg_id>
    send <to> <message> [--kind=request|fyi|response] [--reply-to=<msg_id>]
    done <next> <summary> <request> [--reply-to=<msg_id>]

  Examples:
    sidekar bus who
    sidekar bus requests --status=open
    sidekar bus replies --msg-id=msg_123
    sidekar bus show msg_123
    sidekar bus send claude-2 \"Please review the PR\"
    sidekar bus done claude-2 \"Done\" \"Please take over\""
        }

        "compact" => {
            "\
sidekar compact <classify|filter|run> ...

  RTK-inspired compaction for noisy shell output in agent workflows.

  Subcommands:
    classify <command...>   Show whether Sidekar has a built-in compactor
    filter <command...>     Read raw output from stdin and compact it
    run <command> [args...] Run a command, then compact stdout/stderr

  Examples:
    sidekar compact classify git status
    cargo test 2>&1 | sidekar compact filter cargo test
    sidekar compact run cargo test"
        }

        "monitor" => {
            "\
sidekar monitor <start|stop|status> [tab_id|all]

  Watch one or more tabs for title and favicon changes, then deliver notifications
  through Sidekar's bus transport.

  Examples:
    sidekar monitor start all
    sidekar monitor start 12345 67890
    sidekar monitor status
    sidekar monitor stop"
        }

        "memory" => {
            "\
sidekar memory <write|search|context|observe|sessions|compact|patterns|rate|detail|history> ...

  Local SQLite-backed memory for Sidekar agent sessions.
  Replaces hosted memory/hook flows with in-binary storage and retrieval.

  Subcommands:
    write <type> <summary>                     Store a durable memory (project by default)
    search <query>                             Search memories in current project scope by default
    context                                    Show a scoped startup memory brief
    observe <tool> <summary>                   Append a raw observation
    sessions                                   List recent memory session summaries
    compact                                    Synthesize related project memories
    patterns                                   Promote repeated cross-project patterns
    rate <id> <helpful|wrong|outdated>         Adjust confidence on a memory
    detail <id>                                Show the full memory record
    history <id>                               Show the memory change history

  Examples:
    sidekar memory write convention \"Use Readability.js before scraping article text\"
    sidekar memory write convention \"Use Readability.js\" --scope=global
    sidekar memory search readability
    sidekar memory search readability --scope=all
    sidekar memory context
    sidekar memory compact
    sidekar memory rate 12 helpful
    sidekar memory detail 12"
        }

        "tasks" => {
            "\
sidekar tasks <add|list|done|reopen|delete|show|depend|undepend|deps> ...

  Local SQLite-backed task list with dependency edges.

  Subcommands:
    add <title> [--notes=...] [--priority=N]   Create a task (project by default)
    list [--status=open|done|all] [--ready]    List tasks in current project scope by default
    done <id>                                  Mark task done
    reopen <id>                                Mark task open again
    delete <id>                                Delete a task
    show <id>                                  Show full task details
    depend <task_id> <depends_on_id>           Add a dependency edge
    undepend <task_id> <depends_on_id>         Remove a dependency edge
    deps <id>                                  Show dependency relationships

  Examples:
    sidekar tasks add \"Ship task graph\" --priority=2
    sidekar tasks add \"Renew LLC\" --scope=global
    sidekar tasks list --ready
    sidekar tasks list --scope=all
    sidekar tasks depend 12 8
    sidekar tasks done 8
    sidekar tasks show 12"
        }

        "agent-sessions" => {
            "\
sidekar agent-sessions [show|rename|note] [args] [--limit=N] [--active] [--project=<name>|--all-projects]

  Inspect durable local Sidekar agent session metadata. Lists the current project by default.

  Commands:
    agent-sessions                           List recent sessions for the current project
    agent-sessions --all-projects            List recent sessions across all projects
    agent-sessions --active                  List only still-running sessions
    agent-sessions show <id>                 Show one session in detail
    agent-sessions rename <id> <name>        Set a friendly display name
    agent-sessions note <id> <text>          Store notes on a session
    agent-sessions note <id> --clear         Clear notes

  Examples:
    sidekar agent-sessions
    sidekar agent-sessions --active
    sidekar agent-sessions --all-projects --limit=50
    sidekar agent-sessions show pty:12345:1774750000
    sidekar agent-sessions rename pty:12345:1774750000 \"Frontend worker\"
    sidekar agent-sessions note pty:12345:1774750000 \"Owned the login fix\""
        }

        "repo" => {
            "\
 sidekar repo <pack|tree|changes|actions> [args]

  Zero-config local repo context for agents. Infers the repo root from the current
  directory, respects .gitignore and .ignore, and also reads .sidekarignore.

  Subcommands:
    pack [path]                              Pack repo files to stdout (markdown by default)
    tree [path]                              Show repo tree with estimated token counts
    changes [path]                           Summarize changed files with lightweight symbol hints
    actions [path]                           Discover likely test/lint/build/run actions
    actions run <id> [path]                  Run a discovered action with compact output

  Flags:
    --style=markdown|json|plain              Output format for pack
    --include=glob1,glob2                    Restrict to matching files
    --ignore=glob1,glob2                     Exclude additional files
    --stdin                                  Read explicit file paths from stdin
    --max-file-bytes=N                       Skip files larger than N bytes (default: 1000000)
    --diff                                   Include git worktree and staged diffs
    --logs[=N]                               Include recent git log entries (default: 10)
    --since=<ref>                            Compare changes against a git ref (changes)
    --max-files=N                            Limit reported files in changes (default: 20)
    --max-symbols=N                          Limit symbol hints per changed file (default: 20)
    --timeout=N                              Action timeout in seconds (actions run, default: 120)
    --max-output-chars=N                     Clamp action stdout/stderr (actions run, default: 12000)
    --include-output                         Include action stdout/stderr in the result

  Examples:
    sidekar repo pack
    sidekar repo tree
    sidekar repo changes
    sidekar repo changes --since=origin/main
    sidekar repo actions
    sidekar repo actions run cargo:check
    sidekar repo pack --style=json
    sidekar repo pack --include='src/**,README.md'
    rg --files src | sidekar repo pack --stdin
    sidekar repo pack --diff --logs=5"
        }

        "cron" => {
            "\
sidekar cron <create|list|show|delete> [args...]

  Scheduled job subcommands:
    create <schedule> <action_json|--bash=CMD|--prompt=TEXT> [--target=T] [--name=N] [--once]
    list
    show <job-id>
    delete <job-id>

  Action types:
    {\"tool\":\"screenshot\"}          Run a sidekar tool
    {\"batch\":[...]}                 Run a sequence of tools
    {\"command\":\"echo hello\"}       Run a bash command
    {\"prompt\":\"check status\"}      Inject a prompt into the agent
    --bash=\"echo hello\"             Shorthand for command action
    --prompt=\"check status\"         Shorthand for prompt action

  Examples:
    sidekar cron list
    sidekar cron show c727227a
    sidekar cron create \"*/5 * * * *\" '{\"tool\":\"screenshot\"}'
    sidekar cron create \"*/2 * * * *\" --bash=\"df -h\"
    sidekar cron create \"0 9 * * *\" --prompt=\"check deployment status\"
    sidekar cron create \"0 9 * * *\" --prompt=\"remind me to review PR\" --once
    sidekar cron delete 123abc"
        }

        "loop" => {
            "\
sidekar loop <interval> <prompt> [--once]

  Run a prompt on a recurring interval. Creates a cron job with a prompt
  action that gets injected into the owning agent's PTY.

  Intervals: 2m, 5m, 30m, 1h, 120s (minimum 1 minute)
  Options:
    --once   Fire once then auto-delete

  Examples:
    sidekar loop 5m \"check deployment status\"
    sidekar loop 1h \"summarize recent errors\"
    sidekar loop 10m \"remind me to review the PR\" --once"
        }

        "config" => {
            "\
sidekar config [list|get|set|reset] [key] [value]

  Manage configuration (stored in ~/.sidekar/sidekar.sqlite3).

  Commands:
    config list              Show all settings with defaults
    config get <key>         Get a single setting
    config set <key> <val>   Set a value
    config reset <key>       Revert to default

  Keys: telemetry, feedback, browser, auto_update, relay_pty, max_tabs, cdp_timeout_secs, max_cron_jobs

  Examples:
    sidekar config list
    sidekar config set telemetry false
    sidekar config set relay_pty off
    sidekar config set browser brave
    sidekar config reset browser"
        }

        "telemetry" => {
            "\
sidekar telemetry [on|off|status]

  Enable, disable, or inspect anonymous telemetry collection.

  Examples:
    sidekar telemetry status
    sidekar telemetry off
    sidekar telemetry on"
        }

        "feedback" => {
            "\
sidekar feedback <rating> [comment]

  Send a rating and optional comment to Sidekar.
  Rating must be an integer from 1 to 5.
  Disabled when `sidekar config set feedback false`.

  Examples:
    sidekar feedback 5
    sidekar feedback 3 \"Need better help output for hidden commands\""
        }

        "errors" => {
            "\
sidekar errors [N]

  Show the most recent rows from the local error log stored in SQLite.
  Defaults to 50 rows.

  Examples:
    sidekar errors
    sidekar errors 100"
        }

        "login" => {
            "\
sidekar login

  Authenticate with sidekar.dev using the device auth flow.
  Enables relay-backed sessions, dashboard access, and extension auth."
        }

        "logout" => {
            "\
sidekar logout

  Remove the local device token and clear local encryption state."
        }

        "devices" => {
            "\
sidekar devices

  List registered devices for the authenticated Sidekar account."
        }

        "sessions" => {
            "\
sidekar sessions

  List active Sidekar sessions visible to the authenticated account."
        }

        "daemon" => {
            "\
sidekar daemon [run|stop|restart|status]

  Manage the background Sidekar daemon used by long-running subsystems.

  Examples:
    sidekar daemon
    sidekar daemon status
    sidekar daemon restart
    sidekar daemon stop"
        }

        "totp" => {
            "\
sidekar totp <add|list|get|remove> [args...]

  Store and retrieve TOTP secrets for automated login flows.
  `totp get` prints the current code only, so it is safe to pipe into other commands.

  Examples:
    sidekar totp add github alice BASE32SECRET
    sidekar totp list
    sidekar totp get github alice
    sidekar totp remove 12"
        }

        "pack" => {
            "\
sidekar pack [path|-] [--from=json|yaml|csv]

  PAKT-inspired structured packing for JSON, YAML, or CSV.
  Sidekar replaces repeated keys with a compact dictionary and emits a reversible
  text format that is easier to pass through agent context.

  Examples:
    sidekar pack data.json
    sidekar pack report.yaml
    cat rows.csv | sidekar pack --from=csv"
        }

        "unpack" => {
            "\
sidekar unpack [path|-] [--to=json|yaml|csv]

  Restore Sidekar packed text back to JSON, YAML, or CSV.
  Defaults to the original source format recorded in the packed header.

  Examples:
    sidekar unpack packed.txt
    sidekar unpack packed.txt --to=json
    cat packed.txt | sidekar unpack --to=csv"
        }

        "kv" => {
            "\
sidekar kv <set|get|list|delete> [args...]

  Encrypted key-value storage for secrets and other agent state.

  Examples:
    sidekar kv set github_token abc123
    sidekar kv get github_token
    sidekar kv list
    sidekar kv delete github_token"
        }

        "install" => {
            "\
sidekar install

  Install sidekar skill file for detected agents.
  Detects: Claude Code, Codex, Gemini CLI, OpenCode, Pi."
        }

        "skill" => "sidekar skill\n\n  Print the embedded SKILL.md to stdout (for agents to read).",

        "ext" => {
            "\
sidekar ext <subcommand> [args...]

  Drive your normal Chrome profile via the Sidekar extension. Load unpacked `extension/`
  in Chrome, then click Login with GitHub in the extension popup.

  Use `sidekar --tab <id> ext …` to set tab id when the subcommand omits it; an explicit
  tab id in the subcommand args wins.

  Subcommands: tabs, read [tab_id], screenshot [tab_id], click <target>, type <sel> <text>,
  paste [--html <html>] [--text <text>] [--selector <sel>], set-value <sel> <text>,
  ax-tree [tab_id], eval <js>, eval-page <js>, navigate <url> [tab_id], new-tab [url],
  close [tab_id], scroll [direction], status, stop, install-host [extension_id]

  Examples:
    sidekar ext tabs
    sidekar ext read 3
    sidekar --tab 3 ext screenshot
    sidekar ext click \"#search-btn\"
    sidekar ext paste --html \"<h1>Title</h1>\" --text \"Title\"
    sidekar ext eval-page \"window.monaco?.editor?.getEditors?.()[0]?.getValue()\"
    sidekar ext install-host"
        }

        _ => {
            println!(
                "Unknown command: {command}\n\nRun 'sidekar help' for a list of all commands."
            );
            return;
        }
    };
    println!("{help}");
}

pub fn print_help() {
    println!("{}", crate::cli::render_help(env!("CARGO_PKG_VERSION")));
}
