//! Telegram integration.
//!
//! Treats a Telegram chat as a non-browser "viewer" of a sidekar session.
//! Inbound messages become synthesized keystrokes pushed up the tunnel
//! via `TunnelMsg::Data`. Outbound PTY bytes flow through the registry's
//! standard viewer broadcast; a per-chat task captures them and renders
//! `sendMessage` calls.
//!
//! Wired features:
//!   * `POST /telegram/webhook`   — Telegram update ingress (secret-verified)
//!   * `POST /telegram/deliver`   — internal cross-relay hop for chats
//!                                   whose target session lives elsewhere
//!   * `GET  /telegram/link`      — website mints a one-time link code
//!   * `/start <code>`, `/sessions`, `/here <nick|id>`, `/stop`, `/help`
//!   * per-chat outbound viewer task (ANSI strip, chunk, rate-limit,
//!     turn-boundary flush via `ch:"events"` control frames)
//!   * `update_id` dedup (Mongo TTL collection)
//!
//! Known limitations:
//!   * outbound rendering is a heuristic over raw PTY text — fine for
//!     REPL turns, noisy for long-running TUIs. `/raw` toggle is TODO.

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, RwLock};

use crate::bridge::AppState;
use crate::registry::{Registry, ViewerMsg, ViewerRoute};

// ─── Config ────────────────────────────────────────────────────────

pub const CHATS_COLLECTION: &str = "telegram_chats";
pub const LINK_CODES_COLLECTION: &str = "telegram_link_codes";
pub const SEEN_UPDATES_COLLECTION: &str = "telegram_seen_updates";

/// Chars per outbound Telegram message. Hard limit is 4096 chars; we
/// stay well below to leave room for a trailing "(cont.)" marker.
const TELEGRAM_MSG_LIMIT: usize = 3800;

/// Per-chat minimum gap between outbound messages (Telegram allows ~1
/// msg/sec per chat; stay under to avoid 429).
const PER_CHAT_MIN_GAP: Duration = Duration::from_millis(1200);

/// Quiet window after the last byte before we flush a partial buffer
/// (when no structured turn-complete arrives).
const IDLE_FLUSH: Duration = Duration::from_millis(1200);

/// How long a link code is valid before expiry.
const LINK_CODE_TTL: Duration = Duration::from_secs(600);

/// `update_id` dedup window. Telegram retries for ~24h; we're generous.
const SEEN_UPDATE_TTL: Duration = Duration::from_secs(48 * 3600);

// ─── Env-backed config ─────────────────────────────────────────────

#[derive(Clone)]
pub struct TelegramConfig {
    pub bot_token: String,
    /// Matches the `secret_token` passed to `setWebhook`. Telegram
    /// sends it on every update via `X-Telegram-Bot-Api-Secret-Token`.
    pub webhook_secret: String,
    /// Public bot username (no leading `@`) for display in the link
    /// page and bot-help replies.
    pub bot_username: String,
    /// Shared secret for internal cross-relay `/telegram/deliver`
    /// hops. Must match `RELAY_INTERNAL_SECRET` on peers.
    pub internal_secret: Option<String>,
}

impl TelegramConfig {
    pub fn from_env() -> Option<Self> {
        let bot_token = std::env::var("TELEGRAM_BOT_TOKEN").ok()?;
        let webhook_secret = std::env::var("TELEGRAM_WEBHOOK_SECRET").ok()?;
        let bot_username =
            std::env::var("TELEGRAM_BOT_USERNAME").unwrap_or_else(|_| "sidekar_bot".into());
        let internal_secret = std::env::var("RELAY_INTERNAL_SECRET").ok();
        Some(Self {
            bot_token,
            webhook_secret,
            bot_username,
            internal_secret,
        })
    }
}

// ─── Telegram Bot API client ───────────────────────────────────────

#[derive(Clone)]
pub struct BotClient {
    http: reqwest::Client,
    token: String,
    /// API base URL. Production: `https://api.telegram.org`. Tests
    /// point this at a local mock server to capture outbound messages
    /// without touching the real Telegram API.
    api_base: String,
}

impl BotClient {
    pub fn new(token: String) -> Self {
        Self::with_api_base(token, "https://api.telegram.org".into())
    }

    /// Construct with a custom API base. Intended for tests; production
    /// callers should use [`BotClient::new`].
    pub fn with_api_base(token: String, api_base: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        Self {
            http,
            token,
            api_base,
        }
    }

    pub async fn send_message(&self, chat_id: i64, text: &str) -> Result<(), String> {
        #[derive(Serialize)]
        struct Req<'a> {
            chat_id: i64,
            text: &'a str,
            disable_web_page_preview: bool,
        }
        let url = format!("{}/bot{}/sendMessage", self.api_base, self.token);
        let resp = self
            .http
            .post(&url)
            .json(&Req {
                chat_id,
                text,
                disable_web_page_preview: true,
            })
            .send()
            .await
            .map_err(|e| format!("http: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("telegram {status}: {body}"));
        }
        Ok(())
    }
}

// ─── Per-chat viewer registry (lives in-process) ───────────────────

/// Handle to an active outbound viewer task. Dropping / replacing it
/// signals shutdown to the task.
pub struct ActiveViewer {
    pub session_id: String,
    pub shutdown: tokio::sync::oneshot::Sender<()>,
}

#[derive(Clone)]
pub struct TelegramState {
    pub cfg: TelegramConfig,
    /// chat_id → active viewer. Replacing an entry shuts the old one.
    viewers: Arc<RwLock<HashMap<i64, ActiveViewer>>>,
    /// Per-chat last-send timestamp for outbound rate-limiting.
    pacing: Arc<RwLock<HashMap<i64, Arc<Mutex<Instant>>>>>,
}

impl TelegramState {
    pub fn new(cfg: TelegramConfig) -> Self {
        Self {
            cfg,
            viewers: Arc::new(RwLock::new(HashMap::new())),
            pacing: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn bot(&self) -> BotClient {
        BotClient::new(self.cfg.bot_token.clone())
    }

    async fn pacing_for(&self, chat_id: i64) -> Arc<Mutex<Instant>> {
        {
            let g = self.pacing.read().await;
            if let Some(m) = g.get(&chat_id) {
                return m.clone();
            }
        }
        let mut w = self.pacing.write().await;
        w.entry(chat_id)
            .or_insert_with(|| Arc::new(Mutex::new(Instant::now() - Duration::from_secs(60))))
            .clone()
    }

    /// Stop the current viewer for a chat (if any).
    pub async fn stop_viewer(&self, chat_id: i64) {
        if let Some(old) = self.viewers.write().await.remove(&chat_id) {
            let _ = old.shutdown.send(());
        }
    }

    /// Start (or replace) the outbound viewer for a chat. Idempotent:
    /// calling again with the same session is a no-op.
    pub async fn start_viewer(&self, chat_id: i64, user_id: &str, session_id: &str, registry: &Registry) -> Result<(), String> {
        {
            let g = self.viewers.read().await;
            if let Some(existing) = g.get(&chat_id) {
                if existing.session_id == session_id {
                    return Ok(());
                }
            }
        }
        self.stop_viewer(chat_id).await;

        // Attach as a real viewer — we get the same `ViewerMsg` stream
        // the WebSocket viewer does. Discard the scrollback snapshot;
        // replaying it to Telegram would be spam.
        let (_scrollback, _term_size, rx, _tunnel_tx, viewer_id) = registry
            .add_viewer(session_id, user_id)
            .await
            .ok_or_else(|| "session not found or user mismatch".to_string())?;

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.viewers.write().await.insert(
            chat_id,
            ActiveViewer {
                session_id: session_id.to_string(),
                shutdown: shutdown_tx,
            },
        );

        let pacing = self.pacing_for(chat_id).await;
        let bot = self.bot();
        let registry = registry.clone();
        let session_id_owned = session_id.to_string();
        let viewers = self.viewers.clone();

        tokio::spawn(async move {
            run_viewer(chat_id, bot, pacing, rx, shutdown_rx).await;
            // Clean up the registry side + our own table on exit.
            registry.remove_viewer(&session_id_owned, &viewer_id).await;
            let mut g = viewers.write().await;
            if let Some(v) = g.get(&chat_id) {
                if v.session_id == session_id_owned {
                    g.remove(&chat_id);
                }
            }
        });
        Ok(())
    }
}

// ─── Outbound viewer task ──────────────────────────────────────────

/// Per-chat viewer task. Ingests `ViewerMsg`s from the session and
/// renders Telegram messages.
///
/// Two modes:
///
/// - **Structured** (preferred): sidekar REPL and PTY event parser emit
///   `ch:"events"` frames with `AgentEvent` content (Text, ToolCall,
///   ToolResult, Code, Diff, Status) plus lifecycle markers
///   (turn_start, tool_call_start, assistant_complete, turn_end).
///   Viewer renders directly from those events and ignores the raw
///   byte stream. No ANSI stripping, no chrome, no JSON dumps.
///
/// - **Legacy**: when a session has never emitted any content event
///   (third-party CLIs inside PTY that predate the event parser), we
///   fall back to stripping the raw byte stream and buffering until
///   idle/shutdown.
///
/// Mode is sticky per viewer lifetime. The first content event
/// observed switches to Structured and discards any pending stripped
/// bytes (which would otherwise duplicate what the events carry).
async fn run_viewer(
    chat_id: i64,
    bot: BotClient,
    pacing: Arc<Mutex<Instant>>,
    mut rx: mpsc::UnboundedReceiver<ViewerMsg>,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    let mut buf = String::new();
    let mut flush_deadline: Option<Instant> = None;
    // Only used in Legacy mode; see AnsiStripper doc for why it's stateful.
    let mut stripper = AnsiStripper::new();
    let mut mode = ViewerMode::Legacy;

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            msg = async {
                if let Some(deadline) = flush_deadline {
                    tokio::time::timeout_at(deadline.into(), rx.recv()).await
                } else {
                    Ok(rx.recv().await)
                }
            } => {
                match msg {
                    // idle-flush timer fired
                    Err(_) => {
                        if matches!(mode, ViewerMode::Legacy) {
                            buf.push_str(&stripper.finish());
                        }
                        flush_buf(&bot, chat_id, &pacing, &mut buf).await;
                        flush_deadline = None;
                    }
                    Ok(None) => break,
                    Ok(Some(ViewerMsg::Data(data))) => {
                        // In Structured mode the raw byte stream is ignored:
                        // the events channel is authoritative and bytes
                        // would be a duplicate with ANSI chrome on top.
                        if matches!(mode, ViewerMode::Structured) {
                            continue;
                        }
                        let text = stripper.push(&data);
                        if text.is_empty() {
                            continue;
                        }
                        buf.push_str(&text);
                        flush_deadline = Some(Instant::now() + IDLE_FLUSH);
                        if buf.len() >= TELEGRAM_MSG_LIMIT {
                            flush_buf(&bot, chat_id, &pacing, &mut buf).await;
                            flush_deadline = None;
                        }
                    }
                    Ok(Some(ViewerMsg::Control(text))) => {
                        let parsed = parse_events_frame(&text);
                        match parsed {
                            Some(EventsFrame::Content(ev)) => {
                                // First content event promotes this viewer
                                // to Structured mode. Discard any byte
                                // buffer so we don't double-emit.
                                if matches!(mode, ViewerMode::Legacy) {
                                    mode = ViewerMode::Structured;
                                    buf.clear();
                                    stripper = AnsiStripper::new();
                                }
                                if let Some(rendered) = render_content_event(&ev) {
                                    if !buf.is_empty() && !buf.ends_with('\n') {
                                        buf.push('\n');
                                    }
                                    buf.push_str(&rendered);
                                    flush_deadline = Some(Instant::now() + IDLE_FLUSH);
                                    if buf.len() >= TELEGRAM_MSG_LIMIT {
                                        flush_buf(&bot, chat_id, &pacing, &mut buf).await;
                                        flush_deadline = None;
                                    }
                                }
                            }
                            Some(EventsFrame::Lifecycle(name)) => {
                                if is_boundary_name(&name) {
                                    if matches!(mode, ViewerMode::Legacy) {
                                        buf.push_str(&stripper.finish());
                                    }
                                    flush_buf(&bot, chat_id, &pacing, &mut buf).await;
                                    flush_deadline = None;
                                }
                                // turn_start / tool_call_start / tool_call_end:
                                // no-op for Telegram. Adding "working…"
                                // placeholders would double-message on
                                // assistant_complete.
                            }
                            None => {
                                // Non-events frame (pty resize, etc.) — ignore.
                            }
                        }
                    }
                }
            }
        }
    }
    // Final drain on shutdown.
    if matches!(mode, ViewerMode::Legacy) {
        buf.push_str(&stripper.finish());
    }
    flush_buf(&bot, chat_id, &pacing, &mut buf).await;
}

#[derive(Clone, Copy)]
enum ViewerMode {
    /// No content events have been seen on this viewer; fall back to
    /// stripping raw PTY bytes for human display.
    Legacy,
    /// Session is emitting structured events; render from those and
    /// ignore the byte stream.
    Structured,
}

/// A parsed frame from the viewer's `ViewerMsg::Control` stream. Only
/// frames on `ch:"events"` are interesting; everything else (pty
/// resize, bus routing, etc.) becomes `None`.
#[derive(Debug, Clone)]
enum EventsFrame {
    /// Content event — `event` field is a tagged enum (`AgentEvent`).
    Content(ContentEvent),
    /// Lifecycle marker — `event` field is a bare string.
    Lifecycle(String),
}

/// Subset of sidekar's `AgentEvent` that the Telegram renderer cares
/// about. Kept minimal — new variants fall into `Unknown` and are
/// dropped rather than leaking raw JSON into chat.
#[derive(Debug, Clone)]
enum ContentEvent {
    Text(String),
    ToolCall { tool: String, input: String },
    Code { language: String, content: String },
    Diff { content: String },
    // Status / ToolResult / Unknown — intentionally not in enum; classifier
    // returns None for them so the renderer drops them on the floor.
}

/// Parse a `ViewerMsg::Control` payload into an `EventsFrame`.
///
/// Wire shape (must stay in sync with sidekar's src/events.rs and
/// src/repl/event_forward.rs):
///   Content:   `{"ch":"events","v":1,"event":{"kind":"text","content":"..."}}`
///   Lifecycle: `{"ch":"events","v":1,"event":"assistant_complete"}`
fn parse_events_frame(text: &str) -> Option<EventsFrame> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("ch").and_then(|c| c.as_str()) != Some("events") {
        return None;
    }
    let ev = v.get("event")?;
    if let Some(name) = ev.as_str() {
        return Some(EventsFrame::Lifecycle(name.to_string()));
    }
    let obj = ev.as_object()?;
    let kind = obj.get("kind").and_then(|k| k.as_str())?;
    match kind {
        "text" => {
            let content = obj
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or_default()
                .to_string();
            if content.trim().is_empty() {
                return None;
            }
            Some(EventsFrame::Content(ContentEvent::Text(content)))
        }
        "tool_call" => {
            let tool = obj
                .get("tool")
                .and_then(|t| t.as_str())
                .unwrap_or("tool")
                .to_string();
            let input = obj
                .get("input")
                .and_then(|i| i.as_str())
                .unwrap_or_default()
                .to_string();
            Some(EventsFrame::Content(ContentEvent::ToolCall { tool, input }))
        }
        "code" => {
            let language = obj
                .get("language")
                .and_then(|l| l.as_str())
                .unwrap_or_default()
                .to_string();
            let content = obj
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or_default()
                .to_string();
            if content.trim().is_empty() {
                return None;
            }
            Some(EventsFrame::Content(ContentEvent::Code {
                language,
                content,
            }))
        }
        "diff" => {
            let content = obj
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or_default()
                .to_string();
            if content.trim().is_empty() {
                return None;
            }
            Some(EventsFrame::Content(ContentEvent::Diff { content }))
        }
        // tool_result: intentionally dropped. REPL's forwarder doesn't
        // emit them (Done.message only carries assistant content; tool
        // *results* live on the next turn's user message). PTY's parser
        // heuristically classifies indented output blocks as
        // tool_result but those are usually noisy byproduct of whatever
        // the child CLI printed. Not worth showing on mobile.
        //
        // status: ephemeral; showing it would clutter chat.
        _ => None,
    }
}

/// Whether a lifecycle marker should flush any buffered output.
fn is_boundary_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with("complete")
        || n.ends_with("done")
        || n == "turn_end"
        || n == "assistant_message"
        || n == "error"
}

/// Render a content event as the text to append to the chat buffer.
/// Returns `None` when the event produces no renderable output (empty
/// content after trim, oversized-and-elided, etc.).
fn render_content_event(ev: &ContentEvent) -> Option<String> {
    const MAX_CODE_LINES: usize = 30;
    const MAX_CODE_BYTES: usize = 1200;
    const MAX_TOOL_INPUT_CHARS: usize = 140;

    match ev {
        ContentEvent::Text(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                return None;
            }
            Some(trimmed.to_string())
        }
        ContentEvent::ToolCall { tool, input } => {
            let summary = summarize_tool_input(input, MAX_TOOL_INPUT_CHARS);
            if summary.is_empty() {
                Some(format!("▸ {tool}"))
            } else {
                Some(format!("▸ {tool}: {summary}"))
            }
        }
        ContentEvent::Code { language, content } => {
            let body = truncate_code_block(content, MAX_CODE_LINES, MAX_CODE_BYTES);
            let fence = if language.is_empty() {
                "```".to_string()
            } else {
                format!("```{language}")
            };
            Some(format!("{fence}\n{body}\n```"))
        }
        ContentEvent::Diff { content } => {
            let body = truncate_code_block(content, MAX_CODE_LINES, MAX_CODE_BYTES);
            Some(format!("```diff\n{body}\n```"))
        }
    }
}

/// Compact a tool's argument JSON (or raw string) to a short one-liner
/// suitable for inline chat display. Keeps leading shell commands /
/// file paths readable; strips JSON scaffolding.
fn summarize_tool_input(raw: &str, max_chars: usize) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    // Try to parse as JSON and pick the most informative field. Common
    // sidekar tools expose `command` (Bash), `path` (Read/Write/Edit),
    // `pattern` (Grep/Glob), `args` (Sidekar).
    let summary = if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
        // Field priority: pattern (Grep/Glob) beats path, because for
        // a searching tool the pattern is the action and the path is
        // just the haystack. command (Bash) is unambiguous. url
        // (navigate) and path (Read/Write/Edit) cover file-ish tools.
        let primary = ["command", "pattern", "url", "path"]
            .iter()
            .find_map(|k| v.get(*k).and_then(|x| x.as_str()))
            .map(|s| s.to_string());
        if let Some(s) = primary {
            s
        } else if let Some(args) = v.get("args").and_then(|a| a.as_array()) {
            args.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            // Unknown shape — fall back to the compact JSON string so
            // at least something meaningful surfaces.
            raw.to_string()
        }
    } else {
        raw.to_string()
    };
    // Collapse whitespace to keep the summary single-line.
    let collapsed: String = summary
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    truncate_chars(&collapsed, max_chars)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn truncate_code_block(s: &str, max_lines: usize, max_bytes: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let (body, elided_lines) = if lines.len() > max_lines {
        (
            lines[..max_lines].join("\n"),
            lines.len() - max_lines,
        )
    } else {
        (lines.join("\n"), 0)
    };
    let body = if body.len() > max_bytes {
        let cut = ceil_char_boundary(&body, max_bytes);
        format!("{}…", &body[..cut])
    } else {
        body
    };
    if elided_lines > 0 {
        format!("{body}\n… [{elided_lines} more lines]")
    } else {
        body
    }
}

async fn flush_buf(bot: &BotClient, chat_id: i64, pacing: &Arc<Mutex<Instant>>, buf: &mut String) {
    let text = std::mem::take(buf);
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }
    for chunk in chunk_for_telegram(&text) {
        {
            let mut last = pacing.lock().await;
            let elapsed = last.elapsed();
            if elapsed < PER_CHAT_MIN_GAP {
                tokio::time::sleep(PER_CHAT_MIN_GAP - elapsed).await;
            }
            *last = Instant::now();
        }
        if let Err(e) = bot.send_message(chat_id, &chunk).await {
            tracing::warn!(chat_id, "telegram send failed: {e}");
            return;
        }
    }
}

// ─── Webhook types ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TgUpdate {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<TgMessage>,
}

#[derive(Debug, Deserialize)]
pub struct TgMessage {
    #[allow(dead_code)]
    pub message_id: i64,
    pub chat: TgChat,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TgChat {
    pub id: i64,
}

// ─── Webhook handler ───────────────────────────────────────────────

pub async fn handle_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(update): Json<TgUpdate>,
) -> Response {
    let Some(tg) = state.telegram.clone() else {
        return (StatusCode::NOT_FOUND, "telegram not configured").into_response();
    };

    let secret = headers
        .get("x-telegram-bot-api-secret-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !constant_time_eq(secret.as_bytes(), tg.cfg.webhook_secret.as_bytes()) {
        return (StatusCode::UNAUTHORIZED, "bad secret").into_response();
    }

    // Dedup on update_id (Telegram retries identical update_ids on
    // webhook timeouts/5xx). Insert-if-absent; any success → we've
    // seen it before, drop.
    if !mark_update_seen(&state, update.update_id).await {
        return (StatusCode::OK, "dup").into_response();
    }

    // Fire-and-forget so Telegram never retries on slow processing.
    tokio::spawn(async move {
        if let Err(e) = process_update(&state, &tg, update).await {
            tracing::warn!("telegram update failed: {e}");
        }
    });
    (StatusCode::OK, "ok").into_response()
}

async fn process_update(
    state: &AppState,
    tg: &TelegramState,
    update: TgUpdate,
) -> Result<(), String> {
    let Some(msg) = update.message else {
        return Ok(());
    };
    let chat_id = msg.chat.id;
    let text = msg.text.unwrap_or_default();
    if text.is_empty() {
        return Ok(());
    }
    let bot = tg.bot();

    if let Some(rest) = text.strip_prefix("/start") {
        return handle_start(state, tg, &bot, chat_id, rest.trim()).await;
    }
    if text.starts_with("/sessions") {
        return handle_sessions(state, &bot, chat_id).await;
    }
    if let Some(rest) = text.strip_prefix("/here") {
        return handle_here(state, tg, &bot, chat_id, rest.trim()).await;
    }
    if text.starts_with("/stop") {
        return handle_stop(state, tg, &bot, chat_id).await;
    }
    if text.starts_with("/help") || text == "/?" {
        return handle_help(&bot, &tg.cfg, chat_id).await;
    }
    if text.starts_with('/') {
        let _ = bot
            .send_message(chat_id, "Unknown command. /help for options.")
            .await;
        return Ok(());
    }
    forward_to_session(state, tg, &bot, chat_id, &text).await
}

// ─── Control-command handlers ──────────────────────────────────────

async fn handle_start(
    state: &AppState,
    tg: &TelegramState,
    bot: &BotClient,
    chat_id: i64,
    code: &str,
) -> Result<(), String> {
    if code.is_empty() {
        let _ = bot
            .send_message(
                chat_id,
                "Welcome. To link this chat, visit sidekar.dev → Link Telegram, then send /start <code> here.",
            )
            .await;
        return Ok(());
    }
    let user_id = match redeem_link_code(state, code).await? {
        Some(uid) => uid,
        None => {
            let _ = bot
                .send_message(
                    chat_id,
                    "Invalid or expired code. Generate a new one on sidekar.dev.",
                )
                .await;
            return Ok(());
        }
    };
    upsert_chat(state, chat_id, &user_id, None).await?;
    // Wipe any prior viewer bound to this chat under a different user.
    tg.stop_viewer(chat_id).await;
    let _ = bot
        .send_message(
            chat_id,
            "Linked. Send /sessions to pick a session, or /here <nick> to route.",
        )
        .await;
    Ok(())
}

async fn handle_sessions(state: &AppState, bot: &BotClient, chat_id: i64) -> Result<(), String> {
    let Some(user_id) = chat_user(state, chat_id).await? else {
        let _ = bot.send_message(chat_id, "Not linked. Send /start <code>.").await;
        return Ok(());
    };
    let sessions = state.registry.get_sessions(&user_id).await;
    if sessions.is_empty() {
        let _ = bot
            .send_message(
                chat_id,
                "No live sessions. Run `sidekar` on a machine with a device token.",
            )
            .await;
        return Ok(());
    }
    let mut out = String::from("Live sessions:\n");
    for s in &sessions {
        let nick = s.nickname.as_deref().unwrap_or(&s.name);
        // Short id prefix for users who prefer typing an id.
        let short = &s.id[..s.id.len().min(8)];
        out.push_str(&format!("  {nick}  [{short}]  {}\n", s.cwd));
    }
    out.push_str("\nUse /here <nick> to route (or <id> / short id).");
    let _ = bot.send_message(chat_id, &out).await;
    Ok(())
}

async fn handle_here(
    state: &AppState,
    tg: &TelegramState,
    bot: &BotClient,
    chat_id: i64,
    target: &str,
) -> Result<(), String> {
    let Some(user_id) = chat_user(state, chat_id).await? else {
        let _ = bot.send_message(chat_id, "Not linked. Send /start <code>.").await;
        return Ok(());
    };
    if target.is_empty() {
        let _ = bot
            .send_message(chat_id, "Usage: /here <nick>  (or <session_id>)")
            .await;
        return Ok(());
    }

    let sessions = state.registry.get_sessions(&user_id).await;
    let session_id = match resolve_session_target(&sessions, target) {
        ResolveTarget::Exact(id) => id,
        ResolveTarget::Ambiguous(matches) => {
            let mut msg = format!("Ambiguous '{target}'. Matches:\n");
            for m in matches.iter().take(10) {
                let nick = m.nickname.as_deref().unwrap_or(&m.name);
                let short = &m.id[..m.id.len().min(8)];
                msg.push_str(&format!("  {nick}  [{short}]\n"));
            }
            msg.push_str("\nTry the full id or short id.");
            let _ = bot.send_message(chat_id, &msg).await;
            return Ok(());
        }
        ResolveTarget::None => {
            let _ = bot
                .send_message(chat_id, "No matching session for this account.")
                .await;
            return Ok(());
        }
    };
    let session_id = session_id.as_str();
    upsert_chat(state, chat_id, &user_id, Some(session_id)).await?;

    // Resolve ownership: if local, attach viewer directly. If remote,
    // nothing to do here — inbound messages will hop via
    // /telegram/deliver and the owning relay will attach its own viewer
    // when it receives the first inbound.
    match state
        .registry
        .resolve_viewer_route(session_id, &user_id)
        .await
    {
        Some(ViewerRoute::Local) => {
            if let Err(e) = tg.start_viewer(chat_id, &user_id, session_id, &state.registry).await {
                let _ = bot
                    .send_message(chat_id, &format!("Routed, but viewer attach failed: {e}"))
                    .await;
                return Ok(());
            }
            let _ = bot
                .send_message(chat_id, &format!("Routed to {session_id}. Type to send input."))
                .await;
        }
        Some(ViewerRoute::Remote { owner_origin }) => {
            tg.stop_viewer(chat_id).await;
            let _ = bot
                .send_message(
                    chat_id,
                    &format!("Routed to {session_id} (on {owner_origin})."),
                )
                .await;
        }
        None => {
            let _ = bot.send_message(chat_id, "Session not found or expired.").await;
        }
    }
    Ok(())
}

async fn handle_stop(
    state: &AppState,
    tg: &TelegramState,
    bot: &BotClient,
    chat_id: i64,
) -> Result<(), String> {
    tg.stop_viewer(chat_id).await;
    delete_chat(state, chat_id).await?;
    let _ = bot.send_message(chat_id, "Unlinked.").await;
    Ok(())
}

async fn handle_help(bot: &BotClient, _cfg: &TelegramConfig, chat_id: i64) -> Result<(), String> {
    let _ = bot
        .send_message(
            chat_id,
            "Commands:\n  /start <code>   link this chat\n  /sessions       list live sessions\n  /here <nick>    route messages to a session (nickname or id)\n  /stop           unlink\n\nAnything else is forwarded as keystrokes to the routed session.",
        )
        .await;
    Ok(())
}

// ─── Inbound → tunnel path ─────────────────────────────────────────

async fn forward_to_session(
    state: &AppState,
    tg: &TelegramState,
    bot: &BotClient,
    chat_id: i64,
    text: &str,
) -> Result<(), String> {
    let Some(binding) = chat_binding(state, chat_id).await? else {
        let _ = bot.send_message(chat_id, "Not linked. Send /start <code>.").await;
        return Ok(());
    };
    let Some(session_id) = binding.session_id.clone() else {
        let _ = bot
            .send_message(chat_id, "No session routed. Use /sessions then /here <nick>.")
            .await;
        return Ok(());
    };

    match state
        .registry
        .resolve_viewer_route(&session_id, &binding.user_id)
        .await
    {
        Some(ViewerRoute::Local) => {
            // Make sure the outbound viewer is attached; a link might
            // predate this relay instance's attach.
            if let Err(e) = tg
                .start_viewer(chat_id, &binding.user_id, &session_id, &state.registry)
                .await
            {
                tracing::warn!("viewer reattach failed: {e}");
            }
            deliver_local(state, bot, chat_id, &binding.user_id, &session_id, text).await
        }
        Some(ViewerRoute::Remote { owner_origin }) => {
            deliver_remote(tg, bot, chat_id, &owner_origin, &binding, text).await
        }
        None => {
            let _ = bot.send_message(chat_id, "Session not found or expired.").await;
            Ok(())
        }
    }
}

async fn deliver_local(
    state: &AppState,
    bot: &BotClient,
    chat_id: i64,
    user_id: &str,
    session_id: &str,
    text: &str,
) -> Result<(), String> {
    let mut payload = text.as_bytes().to_vec();
    payload.push(b'\r');
    let pushed = state
        .registry
        .push_tunnel_input(session_id, user_id, payload)
        .await;
    if !pushed {
        let _ = bot
            .send_message(chat_id, "Session unreachable (tunnel disconnected).")
            .await;
    }
    Ok(())
}

async fn deliver_remote(
    tg: &TelegramState,
    bot: &BotClient,
    chat_id: i64,
    owner_origin: &str,
    binding: &ChatBinding,
    text: &str,
) -> Result<(), String> {
    let Some(secret) = tg.cfg.internal_secret.clone() else {
        let _ = bot
            .send_message(
                chat_id,
                "Session lives on another relay instance and cross-relay delivery is disabled (RELAY_INTERNAL_SECRET unset).",
            )
            .await;
        return Ok(());
    };

    let url = format!("{}/telegram/deliver", owner_origin.trim_end_matches('/'));
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("http: {e}"))?;
    let payload = serde_json::json!({
        "chat_id": chat_id,
        "user_id": binding.user_id,
        "session_id": binding.session_id,
        "text": text,
    });
    let resp = http
        .post(&url)
        .header("x-relay-internal", secret)
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("hop: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let _ = bot
            .send_message(
                chat_id,
                &format!("Remote delivery failed: {status} {body}"),
            )
            .await;
    }
    Ok(())
}

// ─── Cross-relay delivery handler ──────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DeliverIn {
    pub chat_id: i64,
    pub user_id: String,
    pub session_id: String,
    pub text: String,
}

/// `POST /telegram/deliver` — internal: another relay instance
/// forwards an inbound Telegram message whose target session is
/// (or was) owned by this instance. Protected by
/// `RELAY_INTERNAL_SECRET` shared across relays. Never exposed to
/// Telegram or the browser.
pub async fn handle_deliver(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DeliverIn>,
) -> Response {
    let Some(tg) = state.telegram.clone() else {
        return (StatusCode::NOT_FOUND, "telegram not configured").into_response();
    };
    let Some(expected) = tg.cfg.internal_secret.clone() else {
        return (StatusCode::FORBIDDEN, "internal delivery disabled").into_response();
    };
    let got = headers
        .get("x-relay-internal")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !constant_time_eq(got.as_bytes(), expected.as_bytes()) {
        return (StatusCode::UNAUTHORIZED, "bad internal secret").into_response();
    }
    // Must actually own the session now.
    match state
        .registry
        .resolve_viewer_route(&body.session_id, &body.user_id)
        .await
    {
        Some(ViewerRoute::Local) => {}
        _ => {
            return (
                StatusCode::CONFLICT,
                "session not owned by this instance",
            )
                .into_response();
        }
    }
    if let Err(e) = tg
        .start_viewer(body.chat_id, &body.user_id, &body.session_id, &state.registry)
        .await
    {
        tracing::warn!("deliver: viewer attach failed: {e}");
    }
    let mut payload = body.text.into_bytes();
    payload.push(b'\r');
    let ok = state
        .registry
        .push_tunnel_input(&body.session_id, &body.user_id, payload)
        .await;
    if ok {
        (StatusCode::OK, "ok").into_response()
    } else {
        (StatusCode::GONE, "tunnel gone").into_response()
    }
}

// ─── Rendering helpers ─────────────────────────────────────────────

/// One-shot ANSI stripper for callers that have the entire byte buffer
/// in hand and need plain UTF-8 text. Convenience wrapper around
/// [`AnsiStripper`]; not suitable for streaming because trailing partial
/// escape sequences or partial UTF-8 tails are force-flushed as junk.
///
/// Previous implementation had two bugs this fixes:
///   * `out.push(b as char)` treated raw bytes as Latin-1 codepoints, so
///     every multi-byte UTF-8 sequence (e.g. `•` = 0xE2 0x80 0xA2) got
///     decoded into three separate codepoints (`â€¢`) — classic mojibake.
///   * Stateless parsing meant an ESC at a chunk boundary was emitted as
///     a bare byte, and the following `[38;2;...m` arrived as plain text
///     with no preceding ESC, so it was shown verbatim.
///
/// Retained as a convenience + test entry point even though the viewer
/// task now uses the streaming API directly.
#[cfg_attr(not(test), allow(dead_code))]
pub fn strip_ansi(data: &[u8]) -> String {
    let mut s = AnsiStripper::new();
    let mut out = s.push(data);
    out.push_str(&s.finish());
    out
}

/// Streaming ANSI stripper + UTF-8 decoder.
///
/// `push(bytes)` returns whatever plain UTF-8 text is ready. Bytes that
/// would be ambiguous in isolation are held back:
///   * A trailing partial escape sequence (`ESC`, `ESC [ … no-final-yet`,
///     `ESC ] … no-terminator-yet`) is buffered until more bytes arrive
///     so it can be consumed rather than mis-emitted.
///   * A trailing partial UTF-8 sequence (a leading byte without its
///     continuation bytes) is buffered so the next chunk can complete
///     the codepoint.
/// Call `finish()` at stream end to force-emit any stuck trailing bytes
/// (as U+FFFD for partial UTF-8; as literal text for a lone trailing
/// ESC, matching terminal behavior of a cancelled sequence).
pub struct AnsiStripper {
    /// Bytes carried over from the previous `push`. Always a prefix of
    /// some ANSI-control metasequence OR a partial UTF-8 codepoint.
    pending: Vec<u8>,
    /// When true, `pending` is a partial ANSI sequence (starts with ESC).
    /// When false, it's at most 3 bytes of a partial UTF-8 codepoint.
    pending_is_ansi: bool,
}

impl AnsiStripper {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            pending_is_ansi: false,
        }
    }

    /// Feed more bytes; return decoded plain text that is safe to emit.
    pub fn push(&mut self, data: &[u8]) -> String {
        // Prepend carryover. Simple and correct; `pending` is tiny (at
        // most a partial escape sequence + a few UTF-8 bytes).
        let mut buf: Vec<u8> = Vec::with_capacity(self.pending.len() + data.len());
        buf.extend_from_slice(&self.pending);
        buf.extend_from_slice(data);
        self.pending.clear();
        self.pending_is_ansi = false;

        // Phase 1: strip ANSI at the byte level. Output is UTF-8 plus
        // any passthrough bytes (which, after the fix, are still raw
        // UTF-8 bytes from the input — we never cast byte→char).
        let mut stripped: Vec<u8> = Vec::with_capacity(buf.len());
        let mut i = 0;
        while i < buf.len() {
            let b = buf[i];
            if b == 0x1b {
                // ESC start. Need at least one more byte to classify.
                if i + 1 >= buf.len() {
                    // Defer the lone ESC to the next push.
                    self.pending.push(0x1b);
                    self.pending_is_ansi = true;
                    break;
                }
                match buf[i + 1] {
                    b'[' => {
                        // CSI: ESC [ params... final(0x40..=0x7E).
                        let mut j = i + 2;
                        let mut found_final = false;
                        while j < buf.len() {
                            let c = buf[j];
                            j += 1;
                            if (0x40..=0x7e).contains(&c) {
                                found_final = true;
                                break;
                            }
                        }
                        if !found_final {
                            // Incomplete — carry entire partial sequence.
                            self.pending.extend_from_slice(&buf[i..]);
                            self.pending_is_ansi = true;
                            break;
                        }
                        i = j;
                        continue;
                    }
                    b']' => {
                        // OSC: ESC ] ... (BEL | ESC \).
                        let mut j = i + 2;
                        let mut found_term = false;
                        while j < buf.len() {
                            if buf[j] == 0x07 {
                                j += 1;
                                found_term = true;
                                break;
                            }
                            if buf[j] == 0x1b {
                                if j + 1 < buf.len() {
                                    if buf[j + 1] == b'\\' {
                                        j += 2;
                                        found_term = true;
                                        break;
                                    }
                                    // Non-ST ESC inside OSC: bail; safer
                                    // to let the outer loop reclassify.
                                    break;
                                } else {
                                    // Dangling ESC at chunk end; hold.
                                    break;
                                }
                            }
                            j += 1;
                        }
                        if !found_term {
                            self.pending.extend_from_slice(&buf[i..]);
                            self.pending_is_ansi = true;
                            break;
                        }
                        i = j;
                        continue;
                    }
                    0x1b => {
                        // ESC ESC — skip the first ESC, reprocess the second.
                        i += 1;
                        continue;
                    }
                    _ => {
                        // Two-byte escape (e.g. ESC c, ESC E, ESC =).
                        i += 2;
                        continue;
                    }
                }
            }
            if b == b'\r' {
                i += 1;
                continue;
            }
            stripped.push(b);
            i += 1;
        }

        // Phase 2: UTF-8 decode. Hold back a trailing partial codepoint
        // so the next push can complete it. `utf8_tail_split` returns
        // the index where a definitely-complete prefix ends.
        let split = utf8_tail_split(&stripped);
        let (ready, tail) = stripped.split_at(split);
        if !tail.is_empty() && !self.pending_is_ansi {
            self.pending.extend_from_slice(tail);
        } else if !tail.is_empty() {
            // Already holding an ANSI partial from earlier this call —
            // impossible because we break the outer loop there, but
            // defensive: emit tail as lossy text rather than lose bytes.
            // Fall through: append tail below via from_utf8_lossy.
        }
        let emit_bytes = if !tail.is_empty() && self.pending_is_ansi {
            // Defensive branch: include tail lossily.
            stripped.as_slice()
        } else {
            ready
        };
        String::from_utf8_lossy(emit_bytes).into_owned()
    }

    /// Flush any leftover partial bytes at stream end.
    pub fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            return String::new();
        }
        let out = if self.pending_is_ansi {
            // Partial escape sequence at EOF — terminal behavior is to
            // discard it entirely. Do the same.
            String::new()
        } else {
            // Partial UTF-8 at EOF — replace with U+FFFD.
            String::from_utf8_lossy(&self.pending).into_owned()
        };
        self.pending.clear();
        self.pending_is_ansi = false;
        out
    }
}

impl Default for AnsiStripper {
    fn default() -> Self {
        Self::new()
    }
}

/// Return the byte index at which `data` can be safely split into a
/// definitely-complete UTF-8 prefix and a (possibly-partial) trailing
/// codepoint. Returns `data.len()` when there is no partial tail.
///
/// A UTF-8 codepoint is 1..=4 bytes: a leading byte followed by 0..=3
/// continuation bytes (0x80..=0xBF). We walk back from the end to find
/// the most recent leading byte; if its expected length exceeds the
/// number of bytes remaining, the tail is partial.
fn utf8_tail_split(data: &[u8]) -> usize {
    let n = data.len();
    if n == 0 {
        return 0;
    }
    // Walk back up to 3 bytes to find the leading byte.
    let max_back = n.min(4);
    for k in 1..=max_back {
        let idx = n - k;
        let b = data[idx];
        if b < 0x80 {
            // ASCII: itself is a full codepoint. Anything after it is
            // either more ASCII or a new codepoint. If k == 1, tail is
            // complete. If k > 1, there are continuation bytes after an
            // ASCII byte — garbage; treat prefix as complete and let
            // lossy decode replace the orphans.
            return n;
        }
        if b & 0xC0 == 0x80 {
            // Continuation byte; keep walking back.
            continue;
        }
        // Leading byte. Determine expected total length.
        let expected = if b & 0xE0 == 0xC0 {
            2
        } else if b & 0xF0 == 0xE0 {
            3
        } else if b & 0xF8 == 0xF0 {
            4
        } else {
            // Invalid leading byte — split here so the orphan is in the
            // tail? Simpler: treat as complete, lossy will substitute.
            return n;
        };
        if k >= expected {
            // Full codepoint already present.
            return n;
        }
        // Partial: split before this leading byte so the caller carries
        // the incomplete codepoint into the next push.
        return idx;
    }
    // More than 3 continuation bytes with no leading byte in sight —
    // malformed. Treat as complete; lossy decode handles it.
    n
}

/// Paragraph-boundary chunker; falls back to char-safe hard split.
pub fn chunk_for_telegram(text: &str) -> Vec<String> {
    if text.len() <= TELEGRAM_MSG_LIMIT {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for para in text.split("\n\n") {
        if current.len() + para.len() + 2 > TELEGRAM_MSG_LIMIT && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        if para.len() > TELEGRAM_MSG_LIMIT {
            let mut start = 0;
            while start < para.len() {
                let end = (start + TELEGRAM_MSG_LIMIT).min(para.len());
                let safe = ceil_char_boundary(para, end);
                out.push(para[start..safe].to_string());
                start = safe;
            }
            continue;
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(para);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn ceil_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

// ─── Session-target resolution ─────────────────────────────────────

enum ResolveTarget {
    Exact(String),
    Ambiguous(Vec<crate::types::SessionInfo>),
    None,
}

/// Match a user-typed string against a list of live sessions in this
/// priority order, returning the first tier that has a hit:
///   1. exact id match
///   2. exact nickname match (case-insensitive)
///   3. exact name match (case-insensitive)
///   4. unique id prefix match (≥4 chars)
///   5. unique case-insensitive nickname/name prefix (≥3 chars)
///
/// Each tier is winner-takes-all: if tier 2 has a unique hit we return
/// it without consulting tier 3. If any single tier has multiple hits
/// we return `Ambiguous` so the user can disambiguate.
fn resolve_session_target(
    sessions: &[crate::types::SessionInfo],
    target: &str,
) -> ResolveTarget {
    let t = target.trim();
    if t.is_empty() {
        return ResolveTarget::None;
    }

    // Tier 1: exact id.
    if let Some(s) = sessions.iter().find(|s| s.id == t) {
        return ResolveTarget::Exact(s.id.clone());
    }

    let t_lower = t.to_ascii_lowercase();

    // Tier 2: exact nickname (case-insensitive).
    let nick_hits: Vec<_> = sessions
        .iter()
        .filter(|s| {
            s.nickname
                .as_deref()
                .map(|n| n.eq_ignore_ascii_case(t))
                .unwrap_or(false)
        })
        .collect();
    match nick_hits.len() {
        1 => return ResolveTarget::Exact(nick_hits[0].id.clone()),
        n if n > 1 => {
            return ResolveTarget::Ambiguous(nick_hits.into_iter().cloned().collect())
        }
        _ => {}
    }

    // Tier 3: exact name (case-insensitive).
    let name_hits: Vec<_> = sessions
        .iter()
        .filter(|s| s.name.eq_ignore_ascii_case(t))
        .collect();
    match name_hits.len() {
        1 => return ResolveTarget::Exact(name_hits[0].id.clone()),
        n if n > 1 => {
            return ResolveTarget::Ambiguous(name_hits.into_iter().cloned().collect())
        }
        _ => {}
    }

    // Tier 4: id prefix (≥4 chars, exact byte prefix).
    if t.len() >= 4 {
        let id_hits: Vec<_> = sessions.iter().filter(|s| s.id.starts_with(t)).collect();
        match id_hits.len() {
            1 => return ResolveTarget::Exact(id_hits[0].id.clone()),
            n if n > 1 => {
                return ResolveTarget::Ambiguous(id_hits.into_iter().cloned().collect())
            }
            _ => {}
        }
    }

    // Tier 5: nickname/name prefix (case-insensitive, ≥3 chars).
    if t.len() >= 3 {
        let pfx_hits: Vec<_> = sessions
            .iter()
            .filter(|s| {
                s.nickname
                    .as_deref()
                    .map(|n| n.to_ascii_lowercase().starts_with(&t_lower))
                    .unwrap_or(false)
                    || s.name.to_ascii_lowercase().starts_with(&t_lower)
            })
            .collect();
        match pfx_hits.len() {
            1 => return ResolveTarget::Exact(pfx_hits[0].id.clone()),
            n if n > 1 => {
                return ResolveTarget::Ambiguous(pfx_hits.into_iter().cloned().collect())
            }
            _ => {}
        }
    }

    ResolveTarget::None
}

// ─── MongoDB helpers ───────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ChatBinding {
    #[allow(dead_code)]
    pub chat_id: i64,
    pub user_id: String,
    pub session_id: Option<String>,
}

async fn chat_binding(state: &AppState, chat_id: i64) -> Result<Option<ChatBinding>, String> {
    let doc = state
        .db
        .collection::<mongodb::bson::Document>(CHATS_COLLECTION)
        .find_one(mongodb::bson::doc! { "chat_id": chat_id })
        .await
        .map_err(|e| format!("mongo: {e}"))?;
    let Some(doc) = doc else {
        return Ok(None);
    };
    let user_id = doc.get_str("user_id").unwrap_or_default().to_string();
    if user_id.is_empty() {
        return Ok(None);
    }
    let session_id = doc.get_str("session_id").ok().map(|s| s.to_string());
    Ok(Some(ChatBinding {
        chat_id,
        user_id,
        session_id,
    }))
}

async fn chat_user(state: &AppState, chat_id: i64) -> Result<Option<String>, String> {
    Ok(chat_binding(state, chat_id).await?.map(|b| b.user_id))
}

async fn upsert_chat(
    state: &AppState,
    chat_id: i64,
    user_id: &str,
    session_id: Option<&str>,
) -> Result<(), String> {
    let now = mongodb::bson::DateTime::now();
    let mut set = mongodb::bson::doc! {
        "chat_id": chat_id,
        "user_id": user_id,
        "updated_at": now,
    };
    if let Some(sid) = session_id {
        set.insert("session_id", sid);
    }
    state
        .db
        .collection::<mongodb::bson::Document>(CHATS_COLLECTION)
        .update_one(
            mongodb::bson::doc! { "chat_id": chat_id },
            mongodb::bson::doc! {
                "$set": set,
                "$setOnInsert": { "created_at": now },
            },
        )
        .upsert(true)
        .await
        .map_err(|e| format!("mongo: {e}"))?;
    Ok(())
}

async fn delete_chat(state: &AppState, chat_id: i64) -> Result<(), String> {
    state
        .db
        .collection::<mongodb::bson::Document>(CHATS_COLLECTION)
        .delete_one(mongodb::bson::doc! { "chat_id": chat_id })
        .await
        .map_err(|e| format!("mongo: {e}"))?;
    Ok(())
}

async fn redeem_link_code(state: &AppState, code: &str) -> Result<Option<String>, String> {
    let coll = state
        .db
        .collection::<mongodb::bson::Document>(LINK_CODES_COLLECTION);
    let doc = coll
        .find_one_and_delete(mongodb::bson::doc! { "code": code })
        .await
        .map_err(|e| format!("mongo: {e}"))?;
    let Some(doc) = doc else {
        return Ok(None);
    };
    let created_at_ms = doc
        .get_datetime("created_at")
        .map(|d| d.timestamp_millis())
        .unwrap_or(0);
    let age_ms = chrono::Utc::now().timestamp_millis() - created_at_ms;
    if age_ms > LINK_CODE_TTL.as_millis() as i64 {
        return Ok(None);
    }
    let user_id = doc.get_str("user_id").unwrap_or_default().to_string();
    if user_id.is_empty() {
        return Ok(None);
    }
    Ok(Some(user_id))
}

/// Insert update_id if unseen. Returns true when this is the first
/// time we've seen it (caller should process); false if already known
/// (caller should drop).
async fn mark_update_seen(state: &AppState, update_id: i64) -> bool {
    let coll = state
        .db
        .collection::<mongodb::bson::Document>(SEEN_UPDATES_COLLECTION);
    // Best-effort GC of expired entries; cheap since collection is small.
    let cutoff = mongodb::bson::DateTime::from_millis(
        chrono::Utc::now().timestamp_millis() - SEEN_UPDATE_TTL.as_millis() as i64,
    );
    let _ = coll
        .delete_many(mongodb::bson::doc! { "seen_at": { "$lt": cutoff } })
        .await;

    let now = mongodb::bson::DateTime::now();
    let res = coll
        .insert_one(mongodb::bson::doc! {
            "update_id": update_id,
            "seen_at": now,
        })
        .await;
    match res {
        Ok(_) => true,
        Err(e) => {
            let msg = e.to_string();
            // Duplicate key from the unique index → already seen.
            if msg.contains("E11000") || msg.contains("duplicate key") {
                false
            } else {
                // On any other mongo error, treat as unseen so we don't
                // silently drop real updates. Logs will show it.
                tracing::warn!("update dedup insert failed: {e}");
                true
            }
        }
    }
}

// ─── Link-code mint endpoint (website-called) ──────────────────────

#[derive(Deserialize)]
pub struct MintQuery {
    pub token: Option<String>,
}

#[derive(Serialize)]
struct MintResp<'a> {
    code: String,
    bot_username: &'a str,
    deep_link: String,
    expires_in_secs: u64,
}

pub async fn handle_mint_link_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<MintQuery>,
) -> Response {
    let Some(tg) = state.telegram.clone() else {
        return (StatusCode::NOT_FOUND, "telegram not configured").into_response();
    };

    let user_id = match resolve_user(&state, &headers, query.token.as_deref()).await {
        Some(uid) => uid,
        None => return (StatusCode::UNAUTHORIZED, "unauthenticated").into_response(),
    };

    let code = generate_code();
    let now = mongodb::bson::DateTime::now();
    if let Err(e) = state
        .db
        .collection::<mongodb::bson::Document>(LINK_CODES_COLLECTION)
        .insert_one(mongodb::bson::doc! {
            "code": &code,
            "user_id": &user_id,
            "created_at": now,
        })
        .await
    {
        tracing::error!("link code insert: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response();
    }

    let deep_link = format!(
        "https://t.me/{}?start={}",
        tg.cfg.bot_username, code
    );
    Json(MintResp {
        code,
        bot_username: &tg.cfg.bot_username,
        deep_link,
        expires_in_secs: LINK_CODE_TTL.as_secs(),
    })
    .into_response()
}

fn generate_code() -> String {
    use rand::Rng;
    const ALPHA: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    (0..8)
        .map(|_| ALPHA[rng.gen_range(0..ALPHA.len())] as char)
        .collect()
}

async fn resolve_user(
    state: &AppState,
    headers: &HeaderMap,
    qtoken: Option<&str>,
) -> Option<String> {
    if let Some(bearer) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
    {
        if let Some(uid) = crate::auth::validate_device_token(&state.db, bearer).await {
            return Some(uid);
        }
    }
    let jwt = qtoken
        .map(|s| s.to_string())
        .or_else(|| cookie(headers, "sidekar_session"))?;
    crate::auth::validate_session_jwt(&jwt, &state.jwt_secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_plain() {
        assert_eq!(strip_ansi(b"hello\n"), "hello\n");
    }

    #[test]
    fn strip_ansi_csi_sgr() {
        // ESC [ 31 m red ESC [ 0 m
        let input = b"\x1b[31mred\x1b[0m";
        assert_eq!(strip_ansi(input), "red");
    }

    #[test]
    fn strip_ansi_osc_bel() {
        // OSC: ESC ] 0 ; title BEL
        let input = b"\x1b]0;title\x07after";
        assert_eq!(strip_ansi(input), "after");
    }

    #[test]
    fn strip_ansi_cursor_moves() {
        let input = b"\x1b[2J\x1b[H\x1b[K\x1b[10;5Hhi";
        assert_eq!(strip_ansi(input), "hi");
    }

    #[test]
    fn strip_ansi_drops_bare_cr() {
        assert_eq!(strip_ansi(b"line\r\nnext"), "line\nnext");
    }

    // ─── Regression: mojibake from bytes-as-Latin-1 cast ───────────

    #[test]
    fn strip_ansi_preserves_utf8_bullet() {
        // • is U+2022, encoded as 0xE2 0x80 0xA2.
        // Old code emitted U+00E2 U+0080 U+00A2 ("â€¢").
        let input = b"\xe2\x80\xa2 item";
        assert_eq!(strip_ansi(input), "• item");
    }

    #[test]
    fn strip_ansi_preserves_utf8_emoji() {
        // 🔥 is U+1F525, encoded as 4 bytes 0xF0 0x9F 0x94 0xA5.
        let input = b"\xf0\x9f\x94\xa5 hot";
        assert_eq!(strip_ansi(input), "🔥 hot");
    }

    #[test]
    fn strip_ansi_utf8_after_ansi() {
        // Common case: SGR color + UTF-8 payload.
        let input = b"\x1b[31m\xe2\x9c\x93 ok\x1b[0m";
        assert_eq!(strip_ansi(input), "✓ ok");
    }

    // ─── Regression: ANSI fragment leak across chunk boundary ──────

    #[test]
    fn stripper_handles_esc_split_across_chunks() {
        // ESC lands at the end of chunk 1; [31m arrives in chunk 2.
        // Stateless strip_ansi would have emitted the ESC as a byte
        // and passed `[31m` through as plain text.
        let mut s = AnsiStripper::new();
        let a = s.push(b"hello\x1b");
        let b = s.push(b"[31mred\x1b[0m done");
        let c = s.finish();
        let full = format!("{a}{b}{c}");
        assert_eq!(full, "hellored done");
    }

    #[test]
    fn stripper_handles_csi_split_mid_sequence() {
        // ESC [ 3 | 1 m  — split between `3` and `1`.
        let mut s = AnsiStripper::new();
        let a = s.push(b"x\x1b[3");
        let b = s.push(b"1mred\x1b[0m");
        let c = s.finish();
        assert_eq!(format!("{a}{b}{c}"), "xred");
    }

    #[test]
    fn stripper_handles_osc_split_before_terminator() {
        // OSC title split before BEL.
        let mut s = AnsiStripper::new();
        let a = s.push(b"\x1b]0;my ti");
        let b = s.push(b"tle\x07after");
        let c = s.finish();
        assert_eq!(format!("{a}{b}{c}"), "after");
    }

    #[test]
    fn stripper_handles_utf8_split_across_chunks() {
        // Multi-byte codepoint split between two pushes.
        // • = 0xE2 0x80 0xA2
        let mut s = AnsiStripper::new();
        let a = s.push(b"a\xe2\x80");
        let b = s.push(b"\xa2 b");
        let c = s.finish();
        assert_eq!(format!("{a}{b}{c}"), "a• b");
    }

    #[test]
    fn stripper_handles_4byte_utf8_split_1_3() {
        // 🔥 = 0xF0 0x9F 0x94 0xA5, split after the leading byte.
        let mut s = AnsiStripper::new();
        let a = s.push(b"\xf0");
        let b = s.push(b"\x9f\x94\xa5!");
        let c = s.finish();
        assert_eq!(format!("{a}{b}{c}"), "🔥!");
    }

    #[test]
    fn stripper_handles_4byte_utf8_split_2_2() {
        let mut s = AnsiStripper::new();
        let a = s.push(b"\xf0\x9f");
        let b = s.push(b"\x94\xa5");
        let c = s.finish();
        assert_eq!(format!("{a}{b}{c}"), "🔥");
    }

    #[test]
    fn stripper_finish_replaces_incomplete_utf8() {
        // Dangling UTF-8 at EOF: expect U+FFFD substitution, not panic.
        let mut s = AnsiStripper::new();
        let a = s.push(b"ok\xe2\x80");
        let b = s.finish();
        let full = format!("{a}{b}");
        assert!(full.starts_with("ok"));
        assert!(full.contains('\u{FFFD}'));
    }

    #[test]
    fn stripper_finish_drops_incomplete_ansi() {
        // Dangling ESC at EOF: terminal semantics — discard.
        let mut s = AnsiStripper::new();
        let a = s.push(b"hi\x1b");
        let b = s.finish();
        assert_eq!(format!("{a}{b}"), "hi");
    }

    #[test]
    fn stripper_one_byte_at_a_time() {
        // Adversarial: feed one byte at a time. All bugs would show up
        // here. Use a mix of ANSI, UTF-8, and plain bytes.
        let full_input: &[u8] =
            b"\x1b[1mHello \xe2\x80\xa2 \xf0\x9f\x94\xa5 world\x1b[0m";
        let mut s = AnsiStripper::new();
        let mut out = String::new();
        for &b in full_input {
            out.push_str(&s.push(&[b]));
        }
        out.push_str(&s.finish());
        assert_eq!(out, "Hello • 🔥 world");
    }

    #[test]
    fn stripper_matches_oneshot_when_buffer_complete() {
        // Streaming result must equal the one-shot result when all
        // bytes arrive in a single push (no partials).
        let cases: &[&[u8]] = &[
            b"plain text",
            b"\x1b[31mred\x1b[0m",
            b"\xe2\x80\xa2 bullet",
            b"\xf0\x9f\x94\xa5 emoji",
            b"mix \x1b[1mbold \xe2\x9c\x93\x1b[0m end",
            b"",
        ];
        for case in cases {
            let one = strip_ansi(case);
            let mut s = AnsiStripper::new();
            let mut streamed = s.push(case);
            streamed.push_str(&s.finish());
            assert_eq!(one, streamed, "mismatch on {case:?}");
        }
    }

    #[test]
    fn chunk_short_is_single() {
        let out = chunk_for_telegram("short message");
        assert_eq!(out, vec!["short message".to_string()]);
    }

    #[test]
    fn chunk_splits_on_paragraph() {
        let para = "a".repeat(TELEGRAM_MSG_LIMIT - 100);
        let text = format!("{para}\n\n{para}");
        let chunks = chunk_for_telegram(&text);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].len() <= TELEGRAM_MSG_LIMIT);
        assert!(chunks[1].len() <= TELEGRAM_MSG_LIMIT);
    }

    #[test]
    fn chunk_hard_splits_oversize_paragraph() {
        let giant = "x".repeat(TELEGRAM_MSG_LIMIT * 2 + 200);
        let chunks = chunk_for_telegram(&giant);
        assert!(chunks.len() >= 3);
        for c in &chunks {
            assert!(c.len() <= TELEGRAM_MSG_LIMIT);
        }
        assert_eq!(chunks.concat(), giant);
    }

    #[test]
    fn chunk_char_boundary_safe() {
        // Force a split inside a multi-byte char run. The splitter
        // must not panic or produce invalid UTF-8.
        let emoji = "🔥"; // 4 bytes
        let giant: String = std::iter::repeat(emoji)
            .take((TELEGRAM_MSG_LIMIT / 4) + 50)
            .collect();
        let chunks = chunk_for_telegram(&giant);
        for c in &chunks {
            assert!(c.is_char_boundary(0) && c.is_char_boundary(c.len()));
        }
        assert_eq!(chunks.concat(), giant);
    }

    // ─── Frame parsing + lifecycle classification ─────────────────

    #[test]
    fn parse_events_frame_lifecycle_string() {
        let frame = r#"{"ch":"events","v":1,"event":"assistant_complete"}"#;
        match parse_events_frame(frame) {
            Some(EventsFrame::Lifecycle(n)) => assert_eq!(n, "assistant_complete"),
            other => panic!("expected Lifecycle, got {other:?}"),
        }
    }

    #[test]
    fn parse_events_frame_content_text() {
        let frame = r#"{"ch":"events","v":1,"event":{"kind":"text","content":"hello"}}"#;
        match parse_events_frame(frame) {
            Some(EventsFrame::Content(ContentEvent::Text(t))) => assert_eq!(t, "hello"),
            other => panic!("expected Text content, got {other:?}"),
        }
    }

    #[test]
    fn parse_events_frame_content_tool_call() {
        let frame = r#"{"ch":"events","v":1,"event":{"kind":"tool_call","tool":"Bash","input":"{\"command\":\"ls\"}"}}"#;
        match parse_events_frame(frame) {
            Some(EventsFrame::Content(ContentEvent::ToolCall { tool, input })) => {
                assert_eq!(tool, "Bash");
                assert!(input.contains("\"command\""));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn parse_events_frame_drops_unknown_kind() {
        // status / tool_result / anything else → None (dropped silently).
        let frame = r#"{"ch":"events","v":1,"event":{"kind":"status","state":"working"}}"#;
        assert!(parse_events_frame(frame).is_none());
        let frame = r#"{"ch":"events","v":1,"event":{"kind":"tool_result","content":"..."}}"#;
        assert!(parse_events_frame(frame).is_none());
    }

    #[test]
    fn parse_events_frame_ignores_other_channels() {
        let frame = r#"{"ch":"pty","event":"resize","cols":80}"#;
        assert!(parse_events_frame(frame).is_none());
    }

    #[test]
    fn parse_events_frame_drops_empty_text() {
        // Whitespace-only text is not useful to emit.
        let frame = r#"{"ch":"events","v":1,"event":{"kind":"text","content":"   "}}"#;
        assert!(parse_events_frame(frame).is_none());
    }

    #[test]
    fn boundary_classification() {
        assert!(is_boundary_name("assistant_complete"));
        assert!(is_boundary_name("turn_end"));
        assert!(is_boundary_name("Stream_Done")); // case-insensitive
        assert!(is_boundary_name("error"));
        assert!(!is_boundary_name("turn_start"));
        assert!(!is_boundary_name("tool_call_start"));
        assert!(!is_boundary_name("tool_call_end")); // ends with _end not turn_end
    }

    // ─── Rendering ───────────────────────────────────────────────

    #[test]
    fn render_text_trims_whitespace() {
        let ev = ContentEvent::Text("  hello  \n".into());
        assert_eq!(render_content_event(&ev), Some("hello".into()));
    }

    #[test]
    fn render_tool_call_prefers_command() {
        let ev = ContentEvent::ToolCall {
            tool: "Bash".into(),
            input: r#"{"command":"ls -la"}"#.into(),
        };
        assert_eq!(render_content_event(&ev), Some("▸ Bash: ls -la".into()));
    }

    #[test]
    fn render_tool_call_prefers_path() {
        let ev = ContentEvent::ToolCall {
            tool: "Read".into(),
            input: r#"{"path":"/tmp/x.rs","offset":10}"#.into(),
        };
        assert_eq!(
            render_content_event(&ev),
            Some("▸ Read: /tmp/x.rs".into())
        );
    }

    #[test]
    fn render_tool_call_joins_args_array() {
        let ev = ContentEvent::ToolCall {
            tool: "Sidekar".into(),
            input: r#"{"args":["memory","list"]}"#.into(),
        };
        assert_eq!(
            render_content_event(&ev),
            Some("▸ Sidekar: memory list".into())
        );
    }

    #[test]
    fn render_tool_call_truncates_long_input() {
        let ev = ContentEvent::ToolCall {
            tool: "Bash".into(),
            input: format!(r#"{{"command":"{}"}}"#, "x".repeat(500)),
        };
        let rendered = render_content_event(&ev).unwrap();
        assert!(rendered.starts_with("▸ Bash: "));
        assert!(rendered.ends_with('…'));
        // Must fit roughly within our cap (chars, not bytes).
        assert!(rendered.chars().count() <= 160);
    }

    #[test]
    fn render_tool_call_falls_back_for_unknown_shape() {
        let ev = ContentEvent::ToolCall {
            tool: "Weird".into(),
            input: r#"{"foo":"bar"}"#.into(),
        };
        // Falls back to the compact JSON itself.
        let r = render_content_event(&ev).unwrap();
        assert!(r.starts_with("▸ Weird:"));
        assert!(r.contains("foo"));
    }

    #[test]
    fn render_code_fences_and_truncates() {
        let big: String = (0..100).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let ev = ContentEvent::Code {
            language: "rust".into(),
            content: big,
        };
        let r = render_content_event(&ev).unwrap();
        assert!(r.starts_with("```rust\n"));
        assert!(r.contains("more lines]"));
        assert!(r.ends_with("```"));
    }

    #[test]
    fn render_diff_always_uses_diff_language() {
        let ev = ContentEvent::Diff {
            content: "- old\n+ new".into(),
        };
        let r = render_content_event(&ev).unwrap();
        assert!(r.starts_with("```diff\n"));
        assert!(r.contains("- old"));
    }

    #[test]
    fn summarize_tool_input_handles_raw_string() {
        // Non-JSON input — echo it back (trimmed, single-lined).
        let s = summarize_tool_input("hello   world", 50);
        assert_eq!(s, "hello world");
    }

    #[test]
    fn summarize_tool_input_empty_returns_empty() {
        assert_eq!(summarize_tool_input("", 50), "");
        assert_eq!(summarize_tool_input("   ", 50), "");
    }

    // ─── run_viewer integration ─────────────────────────────────
    //
    // These spin up a local mock Telegram API (axum) on a random
    // port, point a BotClient at it, drive run_viewer with a scripted
    // ViewerMsg sequence, and assert on the messages the mock
    // captured. They exercise the full pipeline end-to-end —
    // frame parsing, mode transitions, rendering, pacing, and flush —
    // without touching the real Telegram API.

    use axum::{Router, extract::State, routing::post};
    use serde::Deserialize;
    use tokio::net::TcpListener;

    /// Captured messages the mock server received. Shared across the
    /// test so assertions can inspect what was sent.
    #[derive(Clone, Default)]
    struct MockMessages(Arc<std::sync::Mutex<Vec<String>>>);

    impl MockMessages {
        fn take_all(&self) -> Vec<String> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
    }

    #[derive(Deserialize)]
    struct MockSendMessage {
        #[allow(dead_code)]
        chat_id: i64,
        text: String,
    }

    /// Start a mock Telegram API on a random localhost port. Returns
    /// `(api_base_url, captured_messages, shutdown_tx)`. Drop the
    /// shutdown_tx to terminate the server.
    async fn start_mock_telegram() -> (String, MockMessages, tokio::sync::oneshot::Sender<()>) {
        let captured = MockMessages::default();
        let state = captured.clone();
        let app = Router::new()
            .route(
                "/bot{token}/sendMessage",
                post(
                    |State(s): State<MockMessages>,
                     axum::Json(body): axum::Json<MockSendMessage>| async move {
                        s.0.lock().unwrap().push(body.text);
                        axum::Json(serde_json::json!({"ok": true}))
                    },
                ),
            )
            .with_state(state);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });

        (format!("http://{addr}"), captured, shutdown_tx)
    }

    /// Build the harness: mock server + BotClient + viewer task.
    /// Returns the msg sender, shutdown handle, captured messages,
    /// and a JoinHandle for the viewer task.
    struct ViewerHarness {
        tx: mpsc::UnboundedSender<ViewerMsg>,
        shutdown: tokio::sync::oneshot::Sender<()>,
        _mock_shutdown: tokio::sync::oneshot::Sender<()>,
        captured: MockMessages,
        task: tokio::task::JoinHandle<()>,
    }

    impl ViewerHarness {
        async fn start() -> Self {
            let (api_base, captured, mock_shutdown) = start_mock_telegram().await;
            let bot = BotClient::with_api_base("test-token".into(), api_base);
            // Pacing lets all sends through immediately (deadline in the past).
            let pacing = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(1)));
            let (tx, rx) = mpsc::unbounded_channel();
            let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(run_viewer(42, bot, pacing, rx, shutdown_rx));
            Self {
                tx,
                shutdown,
                _mock_shutdown: mock_shutdown,
                captured,
                task,
            }
        }

        fn send(&self, msg: ViewerMsg) {
            self.tx.send(msg).unwrap();
        }

        /// Close the sender and await viewer shutdown so the final
        /// flush completes before we read the captured buffer.
        async fn drain_and_collect(self) -> Vec<String> {
            drop(self.tx);
            // run_viewer finishes when the channel closes; let it do
            // its final flush, then signal shutdown.
            // Small grace window for the final flush HTTP round-trip.
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = self.shutdown.send(());
            let _ = tokio::time::timeout(Duration::from_secs(2), self.task).await;
            self.captured.take_all()
        }
    }

    /// Helper: build a ch:"events" content-text frame.
    fn ev_text(s: &str) -> String {
        serde_json::json!({
            "ch": "events",
            "v": 1,
            "event": {"kind": "text", "content": s},
        })
        .to_string()
    }

    fn ev_tool_call(tool: &str, input: &str) -> String {
        serde_json::json!({
            "ch": "events",
            "v": 1,
            "event": {"kind": "tool_call", "tool": tool, "input": input},
        })
        .to_string()
    }

    fn ev_lifecycle(name: &str) -> String {
        serde_json::json!({
            "ch": "events",
            "v": 1,
            "event": name,
        })
        .to_string()
    }

    #[tokio::test]
    async fn integration_structured_mode_renders_text_and_tool_calls() {
        let h = ViewerHarness::start().await;

        // Simulate a full turn: tool call → assistant text → boundary.
        h.send(ViewerMsg::Control(ev_tool_call(
            "Bash",
            r#"{"command":"ls -la"}"#,
        )));
        h.send(ViewerMsg::Control(ev_text("Here are the results.")));
        h.send(ViewerMsg::Control(ev_lifecycle("assistant_complete")));

        let messages = h.drain_and_collect().await;
        assert_eq!(messages.len(), 1, "expected one flushed message, got {messages:?}");
        let msg = &messages[0];
        assert!(msg.contains("▸ Bash: ls -la"), "missing tool call: {msg}");
        assert!(
            msg.contains("Here are the results."),
            "missing text: {msg}"
        );
        // No ANSI, no JSON dumps.
        assert!(!msg.contains("{\"command\""), "leaked tool JSON: {msg}");
        assert!(!msg.contains("\x1b["), "leaked ANSI: {msg}");
    }

    #[tokio::test]
    async fn integration_content_event_discards_legacy_byte_buffer() {
        // If a Data frame arrives first (legacy mode accumulates it),
        // then a content event arrives, mode flips to Structured and
        // the byte buffer must be discarded — not double-flushed.
        let h = ViewerHarness::start().await;
        h.send(ViewerMsg::Data(b"partial byte banner".to_vec()));
        h.send(ViewerMsg::Control(ev_text("real reply")));
        h.send(ViewerMsg::Control(ev_lifecycle("assistant_complete")));

        let messages = h.drain_and_collect().await;
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert!(msg.contains("real reply"));
        assert!(
            !msg.contains("partial byte banner"),
            "legacy byte buffer leaked into structured output: {msg}"
        );
    }

    #[tokio::test]
    async fn integration_legacy_mode_flushes_bytes_without_events() {
        // PTY sessions with no event emitter: byte stream stripped
        // and flushed on idle.
        let h = ViewerHarness::start().await;
        h.send(ViewerMsg::Data(b"\x1b[31mhello\x1b[0m world\n".to_vec()));
        // No event frame — idle flush (2s) would trigger, but that's
        // too slow for a test. Drop the sender to force final flush.

        let messages = h.drain_and_collect().await;
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.trim_end(), "hello world");
    }

    #[tokio::test]
    async fn integration_multiple_turns_produce_separate_messages() {
        // Each assistant_complete is a flush boundary; two turns → two
        // Telegram messages.
        let h = ViewerHarness::start().await;

        h.send(ViewerMsg::Control(ev_text("Turn one.")));
        h.send(ViewerMsg::Control(ev_lifecycle("assistant_complete")));
        // Must exceed PER_CHAT_MIN_GAP (1.2s) so the second flush
        // isn't paced-delayed past drain_and_collect's grace window.
        tokio::time::sleep(Duration::from_millis(1300)).await;
        h.send(ViewerMsg::Control(ev_text("Turn two.")));
        h.send(ViewerMsg::Control(ev_lifecycle("assistant_complete")));

        let messages = h.drain_and_collect().await;
        assert_eq!(messages.len(), 2, "got {messages:?}");
        assert!(messages[0].contains("Turn one."));
        assert!(messages[1].contains("Turn two."));
    }

    #[tokio::test]
    async fn integration_structured_ignores_raw_bytes_after_flip() {
        // After mode flip, PTY byte output should never appear in
        // Telegram messages. This is the "no ANSI chrome" guarantee.
        let h = ViewerHarness::start().await;
        h.send(ViewerMsg::Control(ev_text("clean reply")));
        // These bytes would be REPL renderer output (status bars,
        // dimmed lines). They MUST be ignored now.
        h.send(ViewerMsg::Data(
            b"\x1b[38;2;105;105;105m[status]\x1b[0m banner\n".to_vec(),
        ));
        h.send(ViewerMsg::Data(b"\xe2\x94\x8c spinner\n".to_vec())); // ┌
        h.send(ViewerMsg::Control(ev_lifecycle("assistant_complete")));

        let messages = h.drain_and_collect().await;
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.trim(), "clean reply");
    }

    #[tokio::test]
    async fn integration_tool_call_summaries_use_preferred_fields() {
        // Regression: each tool type shows the right field.
        let h = ViewerHarness::start().await;
        h.send(ViewerMsg::Control(ev_tool_call(
            "Bash",
            r#"{"command":"cargo test"}"#,
        )));
        h.send(ViewerMsg::Control(ev_tool_call(
            "Read",
            r#"{"path":"/tmp/x.rs","offset":5}"#,
        )));
        h.send(ViewerMsg::Control(ev_tool_call(
            "Grep",
            r#"{"pattern":"fn main","path":"src"}"#,
        )));
        h.send(ViewerMsg::Control(ev_text("done")));
        h.send(ViewerMsg::Control(ev_lifecycle("assistant_complete")));

        let messages = h.drain_and_collect().await;
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert!(msg.contains("▸ Bash: cargo test"));
        assert!(msg.contains("▸ Read: /tmp/x.rs"));
        assert!(msg.contains("▸ Grep: fn main"));
    }

    #[tokio::test]
    async fn integration_utf8_and_emoji_survive_end_to_end() {
        // Regression for the mojibake bug: multi-byte UTF-8 in content
        // events must reach Telegram intact.
        let h = ViewerHarness::start().await;
        h.send(ViewerMsg::Control(ev_text("• bullet 🔥 fire ✓ check")));
        h.send(ViewerMsg::Control(ev_lifecycle("assistant_complete")));

        let messages = h.drain_and_collect().await;
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("• bullet 🔥 fire ✓ check"));
    }

    fn mk_session(id: &str, name: &str, nickname: Option<&str>) -> crate::types::SessionInfo {
        crate::types::SessionInfo {
            id: id.to_string(),
            name: name.to_string(),
            agent_type: "repl".into(),
            cwd: "/".into(),
            hostname: "host".into(),
            nickname: nickname.map(|s| s.to_string()),
            owner_origin: None,
            connected_at: chrono::Utc::now(),
            viewers: 0,
        }
    }

    fn assert_exact(res: ResolveTarget, id: &str) {
        match res {
            ResolveTarget::Exact(got) => assert_eq!(got, id),
            ResolveTarget::Ambiguous(_) => panic!("unexpected ambiguous"),
            ResolveTarget::None => panic!("unexpected none"),
        }
    }

    #[test]
    fn resolve_exact_id() {
        let s = vec![mk_session("abc123xyz", "repl", Some("vizsla"))];
        assert_exact(resolve_session_target(&s, "abc123xyz"), "abc123xyz");
    }

    #[test]
    fn resolve_exact_nickname_case_insensitive() {
        let s = vec![mk_session("abc123xyz", "repl", Some("Vizsla"))];
        assert_exact(resolve_session_target(&s, "vizsla"), "abc123xyz");
        assert_exact(resolve_session_target(&s, "VIZSLA"), "abc123xyz");
    }

    #[test]
    fn resolve_exact_name_when_no_nickname() {
        let s = vec![mk_session("abc123", "my-agent", None)];
        assert_exact(resolve_session_target(&s, "my-agent"), "abc123");
    }

    #[test]
    fn resolve_id_prefix() {
        let s = vec![
            mk_session("abc12345", "repl", Some("vizsla")),
            mk_session("xyz98765", "repl", Some("dunlin")),
        ];
        assert_exact(resolve_session_target(&s, "abc1"), "abc12345");
    }

    #[test]
    fn resolve_nickname_prefix() {
        let s = vec![
            mk_session("abc123", "repl", Some("vizsla")),
            mk_session("xyz456", "repl", Some("dunlin")),
        ];
        assert_exact(resolve_session_target(&s, "viz"), "abc123");
    }

    #[test]
    fn resolve_ambiguous_same_nickname() {
        let s = vec![
            mk_session("abc123", "repl", Some("shared")),
            mk_session("xyz456", "repl", Some("shared")),
        ];
        match resolve_session_target(&s, "shared") {
            ResolveTarget::Ambiguous(hits) => assert_eq!(hits.len(), 2),
            _ => panic!("expected ambiguous"),
        }
    }

    #[test]
    fn resolve_id_beats_nickname_collision() {
        // Session with nickname equal to another session's id: exact
        // id match wins (tier 1 runs first).
        let s = vec![
            mk_session("trick", "repl", Some("vizsla")),
            mk_session("abc123", "repl", Some("trick")),
        ];
        assert_exact(resolve_session_target(&s, "trick"), "trick");
    }

    #[test]
    fn resolve_none_too_short_prefix() {
        let s = vec![mk_session("abc123", "repl", Some("vizsla"))];
        // "ab" is only 2 chars — below the 3-char nickname-prefix gate
        // and 4-char id-prefix gate.
        matches!(resolve_session_target(&s, "ab"), ResolveTarget::None);
    }

    #[test]
    fn resolve_none_no_match() {
        let s = vec![mk_session("abc123", "repl", Some("vizsla"))];
        matches!(resolve_session_target(&s, "zzzzz"), ResolveTarget::None);
    }

    #[test]
    fn resolve_strips_surrounding_whitespace() {
        let s = vec![mk_session("abc123", "repl", Some("vizsla"))];
        assert_exact(resolve_session_target(&s, "  vizsla  "), "abc123");
    }

    #[test]
    fn constant_time_eq_matches_and_mismatches() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get("cookie")?.to_str().ok()?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix(&format!("{name}=")) {
            return Some(v.to_string());
        }
    }
    None
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Ensure MongoDB indexes used by Telegram flows. Safe to call on
/// every startup; creates each index once.
pub async fn ensure_indexes(db: &mongodb::Database) -> Result<(), String> {
    use mongodb::bson::doc;
    use mongodb::{options::IndexOptions, IndexModel};

    let seen: mongodb::Collection<mongodb::bson::Document> = db.collection(SEEN_UPDATES_COLLECTION);
    let seen_idx = IndexModel::builder()
        .keys(doc! { "update_id": 1 })
        .options(IndexOptions::builder().unique(true).build())
        .build();
    seen.create_index(seen_idx)
        .await
        .map_err(|e| format!("seen idx: {e}"))?;

    let chats: mongodb::Collection<mongodb::bson::Document> = db.collection(CHATS_COLLECTION);
    let chats_idx = IndexModel::builder()
        .keys(doc! { "chat_id": 1 })
        .options(IndexOptions::builder().unique(true).build())
        .build();
    chats.create_index(chats_idx)
        .await
        .map_err(|e| format!("chats idx: {e}"))?;

    let codes: mongodb::Collection<mongodb::bson::Document> = db.collection(LINK_CODES_COLLECTION);
    let codes_idx = IndexModel::builder()
        .keys(doc! { "code": 1 })
        .options(IndexOptions::builder().unique(true).build())
        .build();
    codes.create_index(codes_idx)
        .await
        .map_err(|e| format!("codes idx: {e}"))?;

    Ok(())
}
