//! Telegram integration.
//!
//! Treats a Telegram chat as a non-browser "viewer" of a sidekar session.
//! Inbound messages from Telegram become synthesized keystrokes pushed up
//! the existing tunnel `TunnelMsg::Data` path. Outbound PTY bytes flow
//! through the registry's standard viewer broadcast, captured by a per-
//! chat viewer task that renders them as `sendMessage` calls.
//!
//! This file is a scaffold. MVP surface:
//!   * `POST /telegram/webhook`        — Telegram update ingress
//!   * link-code mint + `/start` redeem
//!   * `telegram_chats` Mongo collection with `{ chat_id, user_id,
//!     session_id?, created_at }`
//!   * per-chat outbound viewer task with ANSI strip + paragraph chunk +
//!     naive rate limit
//!
//! The outbound `TelegramViewer`, rendering helpers, and `TgMessage`/
//! `TgUpdate` fields marked `#[allow(dead_code)]` are wired up in the
//! follow-up commit; they're defined here so the shape is reviewable
//! as one unit.
//!
//! Still TODO (tracked in scaffold comments):
//!   * Owner-aware routing when the target session lives on a different
//!     relay instance (hop via internal HTTP or LB-pin by chat_id).
//!   * Smarter outbound rendering off the `ch: "events"` structured
//!     stream instead of raw scrollback.
//!   * Idempotency by `update_id`.

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex};

use crate::bridge::AppState;

// ─── Config ────────────────────────────────────────────────────────

/// Collection name for chat <-> user/session bindings.
pub const CHATS_COLLECTION: &str = "telegram_chats";
/// Collection name for single-use link codes.
pub const LINK_CODES_COLLECTION: &str = "telegram_link_codes";

/// Telegram hard message limit (chars, not bytes, but we chunk bytes
/// conservatively below this).
const TELEGRAM_MSG_LIMIT: usize = 3800;

/// Per-chat minimum gap between outbound messages (Telegram limits to
/// ~1 msg/sec per chat; we stay under that).
const PER_CHAT_MIN_GAP: Duration = Duration::from_millis(1200);

/// How long a link code is valid before it expires.
const LINK_CODE_TTL: Duration = Duration::from_secs(600);

// ─── Env-backed config ─────────────────────────────────────────────

#[derive(Clone)]
pub struct TelegramConfig {
    pub bot_token: String,
    /// Secret token required on `X-Telegram-Bot-Api-Secret-Token` header
    /// for any incoming webhook request. Set when registering the
    /// webhook with `setWebhook`.
    pub webhook_secret: String,
    /// Public bot username (for display in the linking page), e.g.
    /// `@sidekar_bot`. No leading `@`.
    pub bot_username: String,
}

impl TelegramConfig {
    /// Load from env. Returns `None` when Telegram support is not
    /// configured (allows the relay to keep running without it).
    pub fn from_env() -> Option<Self> {
        let bot_token = std::env::var("TELEGRAM_BOT_TOKEN").ok()?;
        let webhook_secret = std::env::var("TELEGRAM_WEBHOOK_SECRET").ok()?;
        let bot_username =
            std::env::var("TELEGRAM_BOT_USERNAME").unwrap_or_else(|_| "sidekar_bot".into());
        Some(Self {
            bot_token,
            webhook_secret,
            bot_username,
        })
    }
}

// ─── Telegram API client ───────────────────────────────────────────

/// Minimal Bot API client. Only implements what we need.
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
            .expect("failed to build reqwest client");
        Self { http, token }
    }

    /// `sendMessage` — plain text, no `parse_mode`. Long messages are
    /// split by the caller.
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

// ─── Webhook types ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TgUpdate {
    #[allow(dead_code)] // used for idempotency in follow-up commit
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<TgMessage>,
}

#[derive(Debug, Deserialize)]
pub struct TgMessage {
    #[allow(dead_code)] // reserved for reply-to threading
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

/// `POST /telegram/webhook` — Telegram pushes updates here.
///
/// Returns 200 quickly; actual work is fire-and-forget on a spawned
/// task so Telegram never retries on slow MongoDB/API calls.
pub async fn handle_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(update): Json<TgUpdate>,
) -> Response {
    let Some(cfg) = state.telegram.clone() else {
        return (StatusCode::NOT_FOUND, "telegram not configured").into_response();
    };

    // Verify secret so Telegram (and only Telegram) can reach us.
    let secret = headers
        .get("x-telegram-bot-api-secret-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if secret != cfg.webhook_secret {
        return (StatusCode::UNAUTHORIZED, "bad secret").into_response();
    }

    // Process asynchronously; always reply 200 so Telegram doesn't retry.
    tokio::spawn(async move {
        if let Err(e) = process_update(&state, &cfg, update).await {
            tracing::warn!("telegram update failed: {e}");
        }
    });

    (StatusCode::OK, "ok").into_response()
}

async fn process_update(
    state: &AppState,
    cfg: &TelegramConfig,
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

    let bot = BotClient::new(cfg.bot_token.clone());

    // Control commands before forwarding as keystrokes.
    if let Some(rest) = text.strip_prefix("/start") {
        return handle_start(state, &bot, chat_id, rest.trim()).await;
    }
    if text.starts_with("/sessions") {
        return handle_sessions(state, &bot, chat_id).await;
    }
    if let Some(rest) = text.strip_prefix("/here") {
        return handle_here(state, &bot, chat_id, rest.trim()).await;
    }
    if text.starts_with("/stop") {
        return handle_stop(state, &bot, chat_id).await;
    }
    if text.starts_with("/help") {
        return handle_help(&bot, cfg, chat_id).await;
    }

    // Forward as keystrokes to the bound session.
    forward_to_session(state, &bot, chat_id, &text).await
}

// ─── Control-command handlers ──────────────────────────────────────

async fn handle_start(
    state: &AppState,
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
                .send_message(chat_id, "Invalid or expired code. Generate a new one on sidekar.dev.")
                .await;
            return Ok(());
        }
    };

    upsert_chat(state, chat_id, &user_id, None).await?;
    let _ = bot
        .send_message(
            chat_id,
            "Linked. Send /sessions to pick a session, or just type to talk to your active one.",
        )
        .await;
    Ok(())
}

async fn handle_sessions(state: &AppState, bot: &BotClient, chat_id: i64) -> Result<(), String> {
    let user_id = match chat_user(state, chat_id).await? {
        Some(u) => u,
        None => {
            let _ = bot.send_message(chat_id, "Not linked. Send /start <code>.").await;
            return Ok(());
        }
    };
    let sessions = state.registry.get_sessions(&user_id).await;
    if sessions.is_empty() {
        let _ = bot
            .send_message(chat_id, "No live sessions. Start one with `sidekar` on your machine.")
            .await;
        return Ok(());
    }
    let mut out = String::from("Live sessions:\n");
    for s in &sessions {
        let nick = s.nickname.as_deref().unwrap_or(&s.name);
        out.push_str(&format!("  {}  ({})\n", s.id, nick));
    }
    out.push_str("\nUse /here <id> to route to one.");
    let _ = bot.send_message(chat_id, &out).await;
    Ok(())
}

async fn handle_here(
    state: &AppState,
    bot: &BotClient,
    chat_id: i64,
    session_id: &str,
) -> Result<(), String> {
    let user_id = match chat_user(state, chat_id).await? {
        Some(u) => u,
        None => {
            let _ = bot.send_message(chat_id, "Not linked. Send /start <code>.").await;
            return Ok(());
        }
    };
    if session_id.is_empty() {
        let _ = bot.send_message(chat_id, "Usage: /here <session_id>").await;
        return Ok(());
    }
    // Validate that the session exists and belongs to this user.
    let owned = state
        .registry
        .get_sessions(&user_id)
        .await
        .iter()
        .any(|s| s.id == session_id);
    if !owned {
        let _ = bot.send_message(chat_id, "No matching session for this account.").await;
        return Ok(());
    }
    upsert_chat(state, chat_id, &user_id, Some(session_id)).await?;
    let _ = bot
        .send_message(chat_id, &format!("Routing to {session_id}. Type to send input."))
        .await;

    // TODO: (re)start the outbound viewer task for this chat/session.
    // See spawn_telegram_viewer stub below.
    Ok(())
}

async fn handle_stop(state: &AppState, bot: &BotClient, chat_id: i64) -> Result<(), String> {
    delete_chat(state, chat_id).await?;
    let _ = bot.send_message(chat_id, "Unlinked.").await;
    Ok(())
}

async fn handle_help(bot: &BotClient, _cfg: &TelegramConfig, chat_id: i64) -> Result<(), String> {
    let _ = bot
        .send_message(
            chat_id,
            "Commands:\n  /start <code>   link this chat\n  /sessions       list live sessions\n  /here <id>      route messages to a session\n  /stop           unlink\n\nAnything else is forwarded as keystrokes to the routed session.",
        )
        .await;
    Ok(())
}

// ─── Inbound → tunnel path ─────────────────────────────────────────

async fn forward_to_session(
    state: &AppState,
    bot: &BotClient,
    chat_id: i64,
    text: &str,
) -> Result<(), String> {
    let binding = match chat_binding(state, chat_id).await? {
        Some(b) => b,
        None => {
            let _ = bot.send_message(chat_id, "Not linked. Send /start <code>.").await;
            return Ok(());
        }
    };

    let Some(session_id) = binding.session_id else {
        let _ = bot
            .send_message(chat_id, "No session routed. Use /sessions then /here <id>.")
            .await;
        return Ok(());
    };

    // Route ownership check — the target session might live on another
    // relay instance. For the MVP, reject with a hint. B.2 would hop
    // via an internal HTTP route.
    match state
        .registry
        .resolve_viewer_route(&session_id, &binding.user_id)
        .await
    {
        Some(crate::registry::ViewerRoute::Local) => {}
        Some(crate::registry::ViewerRoute::Remote { owner_origin }) => {
            let _ = bot
                .send_message(
                    chat_id,
                    &format!("Session lives on {owner_origin}. Multi-relay Telegram routing is not implemented yet."),
                )
                .await;
            return Ok(());
        }
        None => {
            let _ = bot.send_message(chat_id, "Session not found or expired.").await;
            return Ok(());
        }
    }

    // Push the text + newline as keystrokes.
    let mut payload = text.as_bytes().to_vec();
    payload.push(b'\r');
    let pushed = state
        .registry
        .push_tunnel_input(&session_id, &binding.user_id, payload)
        .await;
    if !pushed {
        let _ = bot.send_message(chat_id, "Session unreachable (tunnel disconnected).").await;
    }
    Ok(())
}

// ─── Outbound viewer task (STUB) ───────────────────────────────────

/// State for an active Telegram "viewer" bound to a chat + session.
/// A spawned task owns this and:
///   1. calls `registry.add_viewer()` to receive the scrollback + a
///      `ViewerMsg` channel like any other viewer,
///   2. buffers bytes, strips ANSI, detects turn boundaries,
///   3. sends to Telegram via `sendMessage`, respecting per-chat
///      rate limits.
///
/// This is a scaffold. A follow-up commit wires it up end-to-end and
/// drives rendering off the structured `ch: "events"` stream instead
/// of raw PTY bytes.
#[allow(dead_code)]
pub struct TelegramViewer {
    pub chat_id: i64,
    pub session_id: String,
    pub user_id: String,
    pub bot: BotClient,
    pub last_sent: Arc<Mutex<Instant>>,
}

#[allow(dead_code)]
impl TelegramViewer {
    /// Drain a `ViewerMsg` channel and render to Telegram. Spawn with
    /// `tokio::spawn(viewer.run(rx))`.
    pub async fn run(self, mut rx: mpsc::UnboundedReceiver<crate::registry::ViewerMsg>) {
        let mut buf = String::new();
        let mut flush_deadline: Option<Instant> = None;
        loop {
            let msg = if let Some(deadline) = flush_deadline {
                match tokio::time::timeout_at(deadline.into(), rx.recv()).await {
                    Ok(Some(m)) => m,
                    Ok(None) => break,
                    Err(_) => {
                        self.flush(&mut buf).await;
                        flush_deadline = None;
                        continue;
                    }
                }
            } else {
                match rx.recv().await {
                    Some(m) => m,
                    None => break,
                }
            };
            match msg {
                crate::registry::ViewerMsg::Data(data) => {
                    let text = strip_ansi(&data);
                    if text.is_empty() {
                        continue;
                    }
                    buf.push_str(&text);
                    flush_deadline = Some(Instant::now() + Duration::from_millis(800));
                    if buf.len() >= TELEGRAM_MSG_LIMIT {
                        self.flush(&mut buf).await;
                        flush_deadline = None;
                    }
                }
                crate::registry::ViewerMsg::Control(_text) => {
                    // Control frames: ch=events (turn-complete) or ch=pty
                    // (resize). Turn-complete should trigger an immediate
                    // flush. Parse JSON and switch on `ch`/`event` here.
                    // TODO: drive Telegram rendering off ch:"events"
                    // instead of scrollback heuristics.
                    self.flush(&mut buf).await;
                    flush_deadline = None;
                }
            }
        }
        self.flush(&mut buf).await;
    }

    async fn flush(&self, buf: &mut String) {
        if buf.trim().is_empty() {
            buf.clear();
            return;
        }
        let chunks = chunk_for_telegram(buf);
        for chunk in chunks {
            // Per-chat rate limit.
            let mut last = self.last_sent.lock().await;
            let elapsed = last.elapsed();
            if elapsed < PER_CHAT_MIN_GAP {
                tokio::time::sleep(PER_CHAT_MIN_GAP - elapsed).await;
            }
            if let Err(e) = self.bot.send_message(self.chat_id, &chunk).await {
                tracing::warn!(chat_id = self.chat_id, "telegram send failed: {e}");
                break;
            }
            *last = Instant::now();
        }
        buf.clear();
    }
}

// ─── Rendering helpers ─────────────────────────────────────────────

/// Strip ANSI CSI/OSC escape sequences. Keeps plain text and newlines.
/// Minimal version — covers CSI (`ESC [ … final`) and OSC (`ESC ] … BEL/ST`).
#[allow(dead_code)]
pub fn strip_ansi(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        if b == 0x1b && i + 1 < data.len() {
            match data[i + 1] {
                b'[' => {
                    // CSI: consume until byte in 0x40..=0x7E
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
                    // OSC: consume until BEL (0x07) or ESC \
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
                    // Other ESC-prefixed (e.g. ESC =, ESC >) — skip 2 bytes.
                    i += 2;
                    continue;
                }
            }
        }
        if b == b'\r' {
            // Drop bare CR; keep LF.
            i += 1;
            continue;
        }
        // Pass through ASCII and UTF-8 continuation bytes verbatim.
        out.push(b as char);
        i += 1;
    }
    out
}

/// Split text into Telegram-sized chunks, preferring paragraph boundaries.
#[allow(dead_code)]
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
            // Paragraph alone exceeds the limit — hard-split on byte
            // boundary at char-safe position.
            let mut start = 0;
            while start < para.len() {
                let end = (start + TELEGRAM_MSG_LIMIT).min(para.len());
                let safe_end = ceil_char_boundary(para, end);
                out.push(para[start..safe_end].to_string());
                start = safe_end;
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

#[allow(dead_code)]
fn ceil_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

// ─── MongoDB access: chats + link codes ────────────────────────────

#[derive(Debug, Clone)]
pub struct ChatBinding {
    #[allow(dead_code)] // carried for future routing/logging
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

/// Redeem a one-time link code → user_id. Deletes on success or when
/// expired.
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

// ─── Link-code mint endpoint (website-called) ──────────────────────

#[derive(Deserialize)]
pub struct MintQuery {
    pub token: Option<String>,
}

#[derive(Serialize)]
struct MintResp<'a> {
    code: String,
    bot_username: &'a str,
    expires_in_secs: u64,
}

/// `GET /telegram/link` — called by the sidekar.dev website with a user
/// session JWT. Mints a one-time code the user will DM the bot as
/// `/start <code>`.
pub async fn handle_mint_link_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<MintQuery>,
) -> Response {
    let Some(cfg) = state.telegram.clone() else {
        return (StatusCode::NOT_FOUND, "telegram not configured").into_response();
    };

    // Auth: either bearer device token or session JWT cookie/query.
    let user_id = match resolve_user(&state, &headers, query.token.as_deref()).await {
        Some(uid) => uid,
        None => return (StatusCode::UNAUTHORIZED, "unauthenticated").into_response(),
    };

    let code = generate_code();
    let now = mongodb::bson::DateTime::now();
    let res = state
        .db
        .collection::<mongodb::bson::Document>(LINK_CODES_COLLECTION)
        .insert_one(mongodb::bson::doc! {
            "code": &code,
            "user_id": &user_id,
            "created_at": now,
        })
        .await;
    if let Err(e) = res {
        tracing::error!("failed to insert link code: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response();
    }

    Json(MintResp {
        code,
        bot_username: &cfg.bot_username,
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

async fn resolve_user(state: &AppState, headers: &HeaderMap, qtoken: Option<&str>) -> Option<String> {
    if let Some(bearer) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
    {
        if let Some(uid) = crate::auth::validate_device_token(&state.db, bearer).await {
            return Some(uid);
        }
    }
    // Fall back to session JWT (query or cookie).
    let jwt = qtoken
        .map(|s| s.to_string())
        .or_else(|| cookie(headers, "sidekar_session"))?;
    crate::auth::validate_session_jwt(&jwt, &state.jwt_secret)
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
