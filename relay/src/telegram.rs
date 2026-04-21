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
}

impl BotClient {
    pub fn new(token: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        Self { http, token }
    }

    pub async fn send_message(&self, chat_id: i64, text: &str) -> Result<(), String> {
        #[derive(Serialize)]
        struct Req<'a> {
            chat_id: i64,
            text: &'a str,
            disable_web_page_preview: bool,
        }
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.token);
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

async fn run_viewer(
    chat_id: i64,
    bot: BotClient,
    pacing: Arc<Mutex<Instant>>,
    mut rx: mpsc::UnboundedReceiver<ViewerMsg>,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    let mut buf = String::new();
    let mut flush_deadline: Option<Instant> = None;

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
                        flush_buf(&bot, chat_id, &pacing, &mut buf).await;
                        flush_deadline = None;
                    }
                    Ok(None) => break,
                    Ok(Some(ViewerMsg::Data(data))) => {
                        let text = strip_ansi(&data);
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
                        // Structured events channel: turn boundaries and
                        // pty resize live here. Flush on turn-complete;
                        // ignore everything else.
                        if is_turn_boundary(&text) {
                            flush_buf(&bot, chat_id, &pacing, &mut buf).await;
                            flush_deadline = None;
                        }
                    }
                }
            }
        }
    }
    // Final drain on shutdown.
    flush_buf(&bot, chat_id, &pacing, &mut buf).await;
}

/// Heuristic: treat any `ch:"events"` frame whose `event` field ends
/// with `"complete"` / `"done"` / `"turn_end"` as a flush signal.
/// We're lenient because the REPL's exact vocabulary may evolve.
fn is_turn_boundary(text: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    if v.get("ch").and_then(|c| c.as_str()) != Some("events") {
        return false;
    }
    match v.get("event").and_then(|e| e.as_str()) {
        Some(ev) => {
            let ev = ev.to_ascii_lowercase();
            ev.ends_with("complete")
                || ev.ends_with("done")
                || ev == "turn_end"
                || ev == "assistant_message"
        }
        None => false,
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

/// Strip ANSI CSI/OSC escape sequences; drop bare `\r`.
pub fn strip_ansi(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        if b == 0x1b && i + 1 < data.len() {
            match data[i + 1] {
                b'[' => {
                    // CSI: consume until final byte 0x40..=0x7E
                    i += 2;
                    while i < data.len() {
                        let c = data[i];
                        i += 1;
                        if (0x40..=0x7e).contains(&c) {
                            break;
                        }
                    }
                    continue;
                }
                b']' => {
                    // OSC: consume until BEL or ESC \
                    i += 2;
                    while i < data.len() {
                        if data[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if data[i] == 0x1b && i + 1 < data.len() && data[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                    continue;
                }
                _ => {
                    i += 2;
                    continue;
                }
            }
        }
        if b == b'\r' {
            i += 1;
            continue;
        }
        out.push(b as char);
        i += 1;
    }
    out
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

    #[test]
    fn turn_boundary_detects_complete() {
        let frame = r#"{"ch":"events","event":"assistant_complete","v":1}"#;
        assert!(is_turn_boundary(frame));
    }

    #[test]
    fn turn_boundary_ignores_other_channels() {
        let frame = r#"{"ch":"pty","event":"resize","cols":80}"#;
        assert!(!is_turn_boundary(frame));
    }

    #[test]
    fn turn_boundary_ignores_non_terminal_events() {
        let frame = r#"{"ch":"events","event":"tool_call_start"}"#;
        assert!(!is_turn_boundary(frame));
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
