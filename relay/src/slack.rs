//! Slack integration.
//!
//! Treats a Slack channel as a non-browser "viewer" of a sidekar session.
//! Inbound messages become synthesized keystrokes pushed up the tunnel
//! via `TunnelMsg::Data`. Outbound PTY bytes flow through the registry's
//! standard viewer broadcast; a per-channel task captures them and renders
//! `chat.postMessage` calls.
//!
//! Wired features:
//!   * `POST /slack/events`      — Slack Events API ingress (signature-verified)
//!   * `POST /slack/commands`    — Slack slash-command ingress
//!   * `POST /slack/deliver`     — internal cross-relay hop for channels
//!                                  whose target session lives elsewhere
//!   * `GET  /slack/link`        — website mints a one-time link code
//!   * `/sidekar start <code>`, `/sidekar sessions`, `/sidekar here <nick|id>`,
//!     `/sidekar stop`, `/sidekar help`
//!   * per-channel outbound viewer task (ANSI strip, chunk, rate-limit,
//!     turn-boundary flush via `ch:"events"` control frames)
//!   * `event_id` dedup (Mongo TTL collection)
//!
//! Known limitations:
//!   * outbound rendering is a heuristic over raw PTY text — fine for
//!     REPL turns, noisy for long-running TUIs.

use axum::{
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, RwLock};

use crate::bridge::AppState;
use crate::registry::{Registry, ViewerMsg, ViewerRoute};

// Re-use shared helpers from the telegram module. These are
// rendering/parsing functions that work for any chat transport.
use crate::telegram::{
    parse_events_frame, render_content_event, is_boundary_name, EventsFrame,
    AnsiStripper, ViewerMode,
};

// ─── Config ────────────────────────────────────────────────────────

pub const CHANNELS_COLLECTION: &str = "slack_channels";
pub const LINK_CODES_COLLECTION: &str = "slack_link_codes";
pub const SEEN_EVENTS_COLLECTION: &str = "slack_seen_events";

/// Chars per outbound Slack message. Hard limit is 40000 chars; we
/// stay well below for readability.
const SLACK_MSG_LIMIT: usize = 3800;

/// Per-channel minimum gap between outbound messages (Slack allows ~1
/// msg/sec per channel; stay under to avoid 429).
const PER_CHANNEL_MIN_GAP: Duration = Duration::from_millis(1200);

/// Quiet window after the last byte before we flush a partial buffer
/// (when no structured turn-complete arrives).
const IDLE_FLUSH: Duration = Duration::from_millis(1200);

/// How long a link code is valid before expiry.
const LINK_CODE_TTL: Duration = Duration::from_secs(600);

/// `event_id` dedup window. Slack retries for ~1h; we're generous.
const SEEN_EVENT_TTL: Duration = Duration::from_secs(48 * 3600);

/// Slack request timestamp must be within this window to prevent replay.
const TIMESTAMP_MAX_AGE_SECS: i64 = 300;

// ─── Env-backed config ─────────────────────────────────────────────

#[derive(Clone)]
pub struct SlackConfig {
    /// Bot User OAuth Token (xoxb-...).
    pub bot_token: String,
    /// Signing secret from Slack app settings. Used to verify inbound
    /// requests via `X-Slack-Signature`.
    pub signing_secret: String,
    /// Shared secret for internal cross-relay `/slack/deliver` hops.
    pub internal_secret: Option<String>,
}

impl SlackConfig {
    pub fn from_env() -> Option<Self> {
        let bot_token = std::env::var("SLACK_BOT_TOKEN").ok()?;
        let signing_secret = std::env::var("SLACK_SIGNING_SECRET").ok()?;
        let internal_secret = std::env::var("RELAY_INTERNAL_SECRET").ok();
        Some(Self {
            bot_token,
            signing_secret,
            internal_secret,
        })
    }
}

// ─── Slack Web API client ──────────────────────────────────────────

#[derive(Clone)]
pub struct SlackClient {
    http: reqwest::Client,
    token: String,
    /// API base URL. Production: `https://slack.com`. Tests point this
    /// at a local mock server.
    api_base: String,
}

impl SlackClient {
    pub fn new(token: String) -> Self {
        Self::with_api_base(token, "https://slack.com".into())
    }

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

    pub async fn send_message(&self, channel: &str, text: &str) -> Result<(), String> {
        #[derive(Serialize)]
        struct Req<'a> {
            channel: &'a str,
            text: &'a str,
            unfurl_links: bool,
            unfurl_media: bool,
        }
        let url = format!("{}/api/chat.postMessage", self.api_base);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&Req {
                channel,
                text,
                unfurl_links: false,
                unfurl_media: false,
            })
            .send()
            .await
            .map_err(|e| format!("http: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("slack {status}: {body}"));
        }
        // Slack returns 200 with {"ok": false, "error": "..."} on app errors.
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("json: {e}"))?;
        if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let err = body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown");
            return Err(format!("slack api: {err}"));
        }
        Ok(())
    }

    /// Retrieve the bot's own user ID so we can filter self-messages.
    pub async fn auth_test(&self) -> Result<String, String> {
        let url = format!("{}/api/auth.test", self.api_base);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| format!("http: {e}"))?;
        let body: serde_json::Value = resp.json().await.map_err(|e| format!("json: {e}"))?;
        body.get("user_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "no user_id in auth.test".to_string())
    }
}

// ─── Per-channel viewer registry (lives in-process) ────────────────

pub struct ActiveViewer {
    pub session_id: String,
    pub shutdown: tokio::sync::oneshot::Sender<()>,
}

#[derive(Clone)]
pub struct SlackState {
    pub cfg: SlackConfig,
    /// channel_id → active viewer.
    viewers: Arc<RwLock<HashMap<String, ActiveViewer>>>,
    /// Per-channel last-send timestamp for outbound rate-limiting.
    pacing: Arc<RwLock<HashMap<String, Arc<Mutex<Instant>>>>>,
    /// Bot's own user ID (resolved once at startup via auth.test).
    pub bot_user_id: Arc<RwLock<Option<String>>>,
}

impl SlackState {
    pub fn new(cfg: SlackConfig) -> Self {
        Self {
            cfg,
            viewers: Arc::new(RwLock::new(HashMap::new())),
            pacing: Arc::new(RwLock::new(HashMap::new())),
            bot_user_id: Arc::new(RwLock::new(None)),
        }
    }

    pub fn client(&self) -> SlackClient {
        SlackClient::new(self.cfg.bot_token.clone())
    }

    /// Resolve and cache bot user ID.
    pub async fn resolve_bot_user_id(&self) {
        if self.bot_user_id.read().await.is_some() {
            return;
        }
        match self.client().auth_test().await {
            Ok(uid) => {
                tracing::info!(bot_user_id = %uid, "slack bot user resolved");
                *self.bot_user_id.write().await = Some(uid);
            }
            Err(e) => {
                tracing::warn!("slack auth.test failed: {e}");
            }
        }
    }

    async fn pacing_for(&self, channel: &str) -> Arc<Mutex<Instant>> {
        {
            let g = self.pacing.read().await;
            if let Some(m) = g.get(channel) {
                return m.clone();
            }
        }
        let mut w = self.pacing.write().await;
        w.entry(channel.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(Instant::now() - Duration::from_secs(60))))
            .clone()
    }

    pub async fn stop_viewer(&self, channel: &str) {
        if let Some(old) = self.viewers.write().await.remove(channel) {
            let _ = old.shutdown.send(());
        }
    }

    pub async fn start_viewer(
        &self,
        channel: &str,
        user_id: &str,
        session_id: &str,
        registry: &Registry,
    ) -> Result<(), String> {
        {
            let g = self.viewers.read().await;
            if let Some(existing) = g.get(channel) {
                if existing.session_id == session_id {
                    return Ok(());
                }
            }
        }
        self.stop_viewer(channel).await;

        let (_scrollback, _term_size, rx, _tunnel_tx, viewer_id) = registry
            .add_viewer(session_id, user_id)
            .await
            .ok_or_else(|| "session not found or user mismatch".to_string())?;

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.viewers.write().await.insert(
            channel.to_string(),
            ActiveViewer {
                session_id: session_id.to_string(),
                shutdown: shutdown_tx,
            },
        );

        let pacing = self.pacing_for(channel).await;
        let client = self.client();
        let registry = registry.clone();
        let session_id_owned = session_id.to_string();
        let channel_owned = channel.to_string();
        let viewers = self.viewers.clone();

        tokio::spawn(async move {
            run_viewer(&channel_owned, client, pacing, rx, shutdown_rx).await;
            registry.remove_viewer(&session_id_owned, &viewer_id).await;
            let mut g = viewers.write().await;
            if let Some(v) = g.get(&channel_owned) {
                if v.session_id == session_id_owned {
                    g.remove(&channel_owned);
                }
            }
        });
        Ok(())
    }
}

// ─── Outbound viewer task ──────────────────────────────────────────

async fn run_viewer(
    channel: &str,
    client: SlackClient,
    pacing: Arc<Mutex<Instant>>,
    mut rx: mpsc::UnboundedReceiver<ViewerMsg>,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    let mut buf = String::new();
    let mut flush_deadline: Option<Instant> = None;
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
                    Err(_) => {
                        if matches!(mode, ViewerMode::Legacy) {
                            buf.push_str(&stripper.finish());
                        }
                        flush_buf(&client, channel, &pacing, &mut buf).await;
                        flush_deadline = None;
                    }
                    Ok(None) => break,
                    Ok(Some(ViewerMsg::Data(data))) => {
                        if matches!(mode, ViewerMode::Structured) {
                            continue;
                        }
                        let text = stripper.push(&data);
                        if text.is_empty() {
                            continue;
                        }
                        buf.push_str(&text);
                        flush_deadline = Some(Instant::now() + IDLE_FLUSH);
                        if buf.len() >= SLACK_MSG_LIMIT {
                            flush_buf(&client, channel, &pacing, &mut buf).await;
                            flush_deadline = None;
                        }
                    }
                    Ok(Some(ViewerMsg::Control(text))) => {
                        let parsed = parse_events_frame(&text);
                        match parsed {
                            Some(EventsFrame::Content(ev)) => {
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
                                    if buf.len() >= SLACK_MSG_LIMIT {
                                        flush_buf(&client, channel, &pacing, &mut buf).await;
                                        flush_deadline = None;
                                    }
                                }
                            }
                            Some(EventsFrame::Lifecycle(name)) => {
                                if is_boundary_name(&name) {
                                    if matches!(mode, ViewerMode::Legacy) {
                                        buf.push_str(&stripper.finish());
                                    }
                                    flush_buf(&client, channel, &pacing, &mut buf).await;
                                    flush_deadline = None;
                                }
                            }
                            None => {}
                        }
                    }
                }
            }
        }
    }
    if matches!(mode, ViewerMode::Legacy) {
        buf.push_str(&stripper.finish());
    }
    flush_buf(&client, channel, &pacing, &mut buf).await;
}

async fn flush_buf(
    client: &SlackClient,
    channel: &str,
    pacing: &Arc<Mutex<Instant>>,
    buf: &mut String,
) {
    let text = std::mem::take(buf);
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }
    for chunk in chunk_for_slack(&text) {
        {
            let mut last = pacing.lock().await;
            let elapsed = last.elapsed();
            if elapsed < PER_CHANNEL_MIN_GAP {
                tokio::time::sleep(PER_CHANNEL_MIN_GAP - elapsed).await;
            }
            *last = Instant::now();
        }
        if let Err(e) = client.send_message(channel, &chunk).await {
            tracing::warn!(channel, "slack send failed: {e}");
            return;
        }
    }
}

// ─── Request signature verification ────────────────────────────────

/// Verify Slack's `X-Slack-Signature` header using the signing secret.
/// See: https://api.slack.com/authentication/verifying-requests-from-slack
fn verify_slack_signature(
    signing_secret: &str,
    timestamp: &str,
    body: &[u8],
    signature: &str,
) -> bool {
    // Prevent replay attacks.
    if let Ok(ts) = timestamp.parse::<i64>() {
        let now = chrono::Utc::now().timestamp();
        if (now - ts).abs() > TIMESTAMP_MAX_AGE_SECS {
            return false;
        }
    } else {
        return false;
    }

    let sig_basestring = format!("v0:{timestamp}:{}", String::from_utf8_lossy(body));
    let mut mac =
        Hmac::<Sha256>::new_from_slice(signing_secret.as_bytes()).expect("HMAC key length");
    mac.update(sig_basestring.as_bytes());
    let expected = format!("v0={}", hex::encode(mac.finalize().into_bytes()));

    constant_time_eq(expected.as_bytes(), signature.as_bytes())
}

// ─── Slack Events API types ────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SlackEventPayload {
    #[serde(default)]
    pub r#type: String,
    /// URL verification challenge — Slack sends this once when you
    /// configure the Events API URL.
    #[serde(default)]
    pub challenge: Option<String>,
    /// Token (deprecated verification method; we use signing secret).
    #[serde(default)]
    #[allow(dead_code)]
    pub token: Option<String>,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub event: Option<SlackEvent>,
}

#[derive(Debug, Deserialize)]
pub struct SlackEvent {
    #[serde(default)]
    pub r#type: String,
    /// Channel (or DM) where the message was posted.
    #[serde(default)]
    pub channel: Option<String>,
    /// User who sent the message.
    #[serde(default)]
    pub user: Option<String>,
    /// Message text.
    #[serde(default)]
    pub text: Option<String>,
    /// Bot ID (present if message is from a bot).
    #[serde(default)]
    pub bot_id: Option<String>,
    /// Subtype (e.g. "bot_message"); absent for normal user messages.
    #[serde(default)]
    pub subtype: Option<String>,
}

// ─── Events API handler ───────────────────────────────────────────

pub async fn handle_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(slack) = state.slack.as_ref().cloned() else {
        return (StatusCode::NOT_FOUND, "slack not configured").into_response();
    };

    // Verify signature.
    let timestamp = headers
        .get("x-slack-request-timestamp")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let signature = headers
        .get("x-slack-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !verify_slack_signature(&slack.cfg.signing_secret, timestamp, &body, signature) {
        return (StatusCode::UNAUTHORIZED, "bad signature").into_response();
    }

    // Parse payload.
    let payload: SlackEventPayload = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("slack event parse error: {e}");
            return (StatusCode::BAD_REQUEST, "bad json").into_response();
        }
    };

    // URL verification challenge.
    if payload.r#type == "url_verification" {
        if let Some(challenge) = payload.challenge {
            return Json(serde_json::json!({ "challenge": challenge })).into_response();
        }
        return (StatusCode::BAD_REQUEST, "missing challenge").into_response();
    }

    // Event callback.
    if payload.r#type != "event_callback" {
        return (StatusCode::OK, "ignored").into_response();
    }

    // Dedup on event_id.
    if let Some(event_id) = &payload.event_id {
        if !mark_event_seen(&state, event_id).await {
            return (StatusCode::OK, "dup").into_response();
        }
    }

    // Fire-and-forget so Slack doesn't retry on slow processing.
    tokio::spawn(async move {
        if let Err(e) = process_event(&state, &slack, payload).await {
            tracing::warn!("slack event processing failed: {e}");
        }
    });
    (StatusCode::OK, "ok").into_response()
}

async fn process_event(
    state: &AppState,
    slack: &SlackState,
    payload: SlackEventPayload,
) -> Result<(), String> {
    let Some(event) = payload.event else {
        return Ok(());
    };

    // Only handle user messages (not bot messages, not subtypes).
    if event.r#type != "message" && event.r#type != "app_mention" {
        return Ok(());
    }
    if event.subtype.is_some() || event.bot_id.is_some() {
        return Ok(());
    }

    // Filter out messages from ourselves.
    if let Some(bot_uid) = slack.bot_user_id.read().await.as_ref() {
        if event.user.as_deref() == Some(bot_uid.as_str()) {
            return Ok(());
        }
    }

    let channel = event.channel.unwrap_or_default();
    let text = event.text.unwrap_or_default();
    if channel.is_empty() || text.is_empty() {
        return Ok(());
    }

    let client = slack.client();

    // Strip bot mention prefix if present (e.g. "<@U12345> command").
    let text = strip_mention_prefix(&text);

    // Route commands.
    if let Some(rest) = text.strip_prefix("start ").or_else(|| {
        if text == "start" {
            Some("")
        } else {
            None
        }
    }) {
        return handle_start(state, slack, &client, &channel, rest.trim()).await;
    }
    if text == "sessions" || text.starts_with("sessions ") {
        return handle_sessions(state, &client, &channel).await;
    }
    if let Some(rest) = text.strip_prefix("here ").or_else(|| {
        if text == "here" {
            Some("")
        } else {
            None
        }
    }) {
        return handle_here(state, slack, &client, &channel, rest.trim()).await;
    }
    if text == "stop" {
        return handle_stop(state, slack, &client, &channel).await;
    }
    if text == "help" || text == "?" {
        return handle_help(&client, &channel).await;
    }

    // Not a command → forward to session.
    forward_to_session(state, slack, &client, &channel, &text).await
}

/// Strip a leading `<@Uxxxx>` mention (with optional trailing space).
fn strip_mention_prefix(text: &str) -> &str {
    let t = text.trim();
    if t.starts_with("<@") {
        if let Some(end) = t.find('>') {
            let rest = &t[end + 1..];
            return rest.trim_start();
        }
    }
    t
}

// ─── Slash command handler ─────────────────────────────────────────

/// `POST /slack/commands` — handles `/sidekar <subcommand>` slash commands.
pub async fn handle_slash_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(slack) = state.slack.as_ref().cloned() else {
        return (StatusCode::NOT_FOUND, "slack not configured").into_response();
    };

    let timestamp = headers
        .get("x-slack-request-timestamp")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let signature = headers
        .get("x-slack-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !verify_slack_signature(&slack.cfg.signing_secret, timestamp, &body, signature) {
        return (StatusCode::UNAUTHORIZED, "bad signature").into_response();
    }

    // Slack slash commands are URL-encoded form data.
    let params: HashMap<String, String> =
        serde_urlencoded::from_bytes(&body).unwrap_or_default();

    let channel = params.get("channel_id").cloned().unwrap_or_default();
    let text = params.get("text").cloned().unwrap_or_default();
    let text = text.trim().to_string();

    if channel.is_empty() {
        return Json(serde_json::json!({
            "response_type": "ephemeral",
            "text": "Could not determine channel."
        }))
        .into_response();
    }

    // Process in background; return immediate ack.
    let state2 = state.clone();
    let slack2 = slack.clone();
    let channel2 = channel.clone();
    tokio::spawn(async move {
        let client = slack2.client();
        let result = match text.split_once(' ').map(|(cmd, rest)| (cmd, rest.trim())) {
            _ if text.is_empty() || text == "help" || text == "?" => {
                handle_help(&client, &channel2).await
            }
            Some(("start", code)) => {
                handle_start(&state2, &slack2, &client, &channel2, code).await
            }
            _ if text == "start" => {
                handle_start(&state2, &slack2, &client, &channel2, "").await
            }
            _ if text == "sessions" => handle_sessions(&state2, &client, &channel2).await,
            Some(("here", target)) => {
                handle_here(&state2, &slack2, &client, &channel2, target).await
            }
            _ if text == "here" => {
                handle_here(&state2, &slack2, &client, &channel2, "").await
            }
            _ if text == "stop" => handle_stop(&state2, &slack2, &client, &channel2).await,
            _ => {
                let _ = client
                    .send_message(&channel2, &format!("Unknown command: {text}\nUse `/sidekar help` for options."))
                    .await;
                Ok(())
            }
        };
        if let Err(e) = result {
            tracing::warn!("slack slash command failed: {e}");
        }
    });

    // Immediate empty 200 so Slack doesn't show "command failed".
    (StatusCode::OK, "").into_response()
}

// ─── Control-command handlers ──────────────────────────────────────

async fn handle_start(
    state: &AppState,
    slack: &SlackState,
    client: &SlackClient,
    channel: &str,
    code: &str,
) -> Result<(), String> {
    if code.is_empty() {
        let _ = client
            .send_message(
                channel,
                "Welcome. To link this channel, visit sidekar.dev → Link Slack, then use `start <code>` here.",
            )
            .await;
        return Ok(());
    }
    let user_id = match redeem_link_code(state, code).await? {
        Some(uid) => uid,
        None => {
            let _ = client
                .send_message(
                    channel,
                    "Invalid or expired code. Generate a new one on sidekar.dev.",
                )
                .await;
            return Ok(());
        }
    };
    upsert_channel(state, channel, &user_id, None).await?;
    slack.stop_viewer(channel).await;
    let _ = client
        .send_message(
            channel,
            "Linked. Send `sessions` to pick a session, or `here <nick>` to route.",
        )
        .await;
    Ok(())
}

async fn handle_sessions(
    state: &AppState,
    client: &SlackClient,
    channel: &str,
) -> Result<(), String> {
    let Some(user_id) = channel_user(state, channel).await? else {
        let _ = client
            .send_message(channel, "Not linked. Use `start <code>`.")
            .await;
        return Ok(());
    };
    let sessions = state.registry.get_sessions(&user_id).await;
    if sessions.is_empty() {
        let _ = client
            .send_message(
                channel,
                "No live sessions. Run `sidekar` on a machine with a device token.",
            )
            .await;
        return Ok(());
    }
    let mut out = String::from("Live sessions:\n");
    for s in &sessions {
        let nick = s.nickname.as_deref().unwrap_or(&s.name);
        let short = &s.id[..s.id.len().min(8)];
        out.push_str(&format!("  `{nick}`  `[{short}]`  {}\n", s.cwd));
    }
    out.push_str("\nUse `here <nick>` to route (or `<id>` / short id).");
    let _ = client.send_message(channel, &out).await;
    Ok(())
}

async fn handle_here(
    state: &AppState,
    slack: &SlackState,
    client: &SlackClient,
    channel: &str,
    target: &str,
) -> Result<(), String> {
    let Some(user_id) = channel_user(state, channel).await? else {
        let _ = client
            .send_message(channel, "Not linked. Use `start <code>`.")
            .await;
        return Ok(());
    };
    if target.is_empty() {
        let _ = client
            .send_message(channel, "Usage: `here <nick>` (or `<session_id>`)")
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
                msg.push_str(&format!("  `{nick}`  `[{short}]`\n"));
            }
            msg.push_str("\nTry the full id or short id.");
            let _ = client.send_message(channel, &msg).await;
            return Ok(());
        }
        ResolveTarget::None => {
            let _ = client
                .send_message(channel, "No matching session for this account.")
                .await;
            return Ok(());
        }
    };
    let session_id = session_id.as_str();
    upsert_channel(state, channel, &user_id, Some(session_id)).await?;

    match state
        .registry
        .resolve_viewer_route(session_id, &user_id)
        .await
    {
        Some(ViewerRoute::Local) => {
            if let Err(e) = slack
                .start_viewer(channel, &user_id, session_id, &state.registry)
                .await
            {
                let _ = client
                    .send_message(channel, &format!("Routed, but viewer attach failed: {e}"))
                    .await;
                return Ok(());
            }
            let _ = client
                .send_message(
                    channel,
                    &format!("Routed to `{session_id}`. Type to send input."),
                )
                .await;
        }
        Some(ViewerRoute::Remote { owner_origin }) => {
            slack.stop_viewer(channel).await;
            let _ = client
                .send_message(
                    channel,
                    &format!("Routed to `{session_id}` (on {owner_origin})."),
                )
                .await;
        }
        None => {
            let _ = client
                .send_message(channel, "Session not found or expired.")
                .await;
        }
    }
    Ok(())
}

async fn handle_stop(
    state: &AppState,
    slack: &SlackState,
    client: &SlackClient,
    channel: &str,
) -> Result<(), String> {
    slack.stop_viewer(channel).await;
    delete_channel(state, channel).await?;
    let _ = client.send_message(channel, "Unlinked.").await;
    Ok(())
}

async fn handle_help(client: &SlackClient, channel: &str) -> Result<(), String> {
    let _ = client
        .send_message(
            channel,
            "Commands:\n  `start <code>`   — link this channel\n  `sessions`       — list live sessions\n  `here <nick>`    — route messages to a session (nickname or id)\n  `stop`           — unlink\n\nAnything else is forwarded as keystrokes to the routed session.\n\nUse `/sidekar <command>` or mention the bot with a command.",
        )
        .await;
    Ok(())
}

// ─── Inbound → tunnel path ─────────────────────────────────────────

async fn forward_to_session(
    state: &AppState,
    slack: &SlackState,
    client: &SlackClient,
    channel: &str,
    text: &str,
) -> Result<(), String> {
    let Some(binding) = channel_binding(state, channel).await? else {
        let _ = client
            .send_message(channel, "Not linked. Use `start <code>`.")
            .await;
        return Ok(());
    };
    let Some(session_id) = binding.session_id.clone() else {
        let _ = client
            .send_message(
                channel,
                "No session routed. Use `sessions` then `here <nick>`.",
            )
            .await;
        return Ok(());
    };

    match state
        .registry
        .resolve_viewer_route(&session_id, &binding.user_id)
        .await
    {
        Some(ViewerRoute::Local) => {
            if let Err(e) = slack
                .start_viewer(channel, &binding.user_id, &session_id, &state.registry)
                .await
            {
                tracing::warn!("viewer reattach failed: {e}");
            }
            deliver_local(state, client, channel, &binding.user_id, &session_id, text).await
        }
        Some(ViewerRoute::Remote { owner_origin }) => {
            deliver_remote(slack, client, channel, &owner_origin, &binding, text).await
        }
        None => {
            let _ = client
                .send_message(channel, "Session not found or expired.")
                .await;
            Ok(())
        }
    }
}

async fn deliver_local(
    state: &AppState,
    client: &SlackClient,
    channel: &str,
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
        let _ = client
            .send_message(channel, "Session unreachable (tunnel disconnected).")
            .await;
    }
    Ok(())
}

async fn deliver_remote(
    slack: &SlackState,
    client: &SlackClient,
    channel: &str,
    owner_origin: &str,
    binding: &ChannelBinding,
    text: &str,
) -> Result<(), String> {
    let Some(secret) = slack.cfg.internal_secret.clone() else {
        let _ = client
            .send_message(
                channel,
                "Session lives on another relay instance and cross-relay delivery is disabled (RELAY_INTERNAL_SECRET unset).",
            )
            .await;
        return Ok(());
    };

    let url = format!("{}/slack/deliver", owner_origin.trim_end_matches('/'));
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("http: {e}"))?;
    let payload = serde_json::json!({
        "channel": channel,
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
        let _ = client
            .send_message(
                channel,
                &format!("Remote delivery failed: {status} {body}"),
            )
            .await;
    }
    Ok(())
}

// ─── Cross-relay delivery handler ──────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DeliverIn {
    pub channel: String,
    pub user_id: String,
    pub session_id: String,
    pub text: String,
}

pub async fn handle_deliver(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DeliverIn>,
) -> Response {
    let Some(slack) = state.slack.as_ref().cloned() else {
        return (StatusCode::NOT_FOUND, "slack not configured").into_response();
    };
    let Some(expected) = slack.cfg.internal_secret.clone() else {
        return (StatusCode::FORBIDDEN, "internal delivery disabled").into_response();
    };
    let got = headers
        .get("x-relay-internal")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !constant_time_eq(got.as_bytes(), expected.as_bytes()) {
        return (StatusCode::UNAUTHORIZED, "bad internal secret").into_response();
    }
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
    if let Err(e) = slack
        .start_viewer(&body.channel, &body.user_id, &body.session_id, &state.registry)
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

// ─── Message chunker ──────────────────────────────────────────────

pub fn chunk_for_slack(text: &str) -> Vec<String> {
    if text.len() <= SLACK_MSG_LIMIT {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for para in text.split("\n\n") {
        if current.len() + para.len() + 2 > SLACK_MSG_LIMIT && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        if para.len() > SLACK_MSG_LIMIT {
            let mut start = 0;
            while start < para.len() {
                let end = (start + SLACK_MSG_LIMIT).min(para.len());
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

fn resolve_session_target(
    sessions: &[crate::types::SessionInfo],
    target: &str,
) -> ResolveTarget {
    let t = target.trim();
    if t.is_empty() {
        return ResolveTarget::None;
    }

    if let Some(s) = sessions.iter().find(|s| s.id == t) {
        return ResolveTarget::Exact(s.id.clone());
    }

    let t_lower = t.to_ascii_lowercase();

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
pub struct ChannelBinding {
    #[allow(dead_code)]
    pub channel: String,
    pub user_id: String,
    pub session_id: Option<String>,
}

async fn channel_binding(
    state: &AppState,
    channel: &str,
) -> Result<Option<ChannelBinding>, String> {
    let doc = state
        .db
        .collection::<mongodb::bson::Document>(CHANNELS_COLLECTION)
        .find_one(mongodb::bson::doc! { "channel": channel })
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
    Ok(Some(ChannelBinding {
        channel: channel.to_string(),
        user_id,
        session_id,
    }))
}

async fn channel_user(state: &AppState, channel: &str) -> Result<Option<String>, String> {
    Ok(channel_binding(state, channel).await?.map(|b| b.user_id))
}

async fn upsert_channel(
    state: &AppState,
    channel: &str,
    user_id: &str,
    session_id: Option<&str>,
) -> Result<(), String> {
    let now = mongodb::bson::DateTime::now();
    let mut set = mongodb::bson::doc! {
        "channel": channel,
        "user_id": user_id,
        "updated_at": now,
    };
    if let Some(sid) = session_id {
        set.insert("session_id", sid);
    }
    state
        .db
        .collection::<mongodb::bson::Document>(CHANNELS_COLLECTION)
        .update_one(
            mongodb::bson::doc! { "channel": channel },
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

async fn delete_channel(state: &AppState, channel: &str) -> Result<(), String> {
    state
        .db
        .collection::<mongodb::bson::Document>(CHANNELS_COLLECTION)
        .delete_one(mongodb::bson::doc! { "channel": channel })
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

async fn mark_event_seen(state: &AppState, event_id: &str) -> bool {
    let coll = state
        .db
        .collection::<mongodb::bson::Document>(SEEN_EVENTS_COLLECTION);
    let cutoff = mongodb::bson::DateTime::from_millis(
        chrono::Utc::now().timestamp_millis() - SEEN_EVENT_TTL.as_millis() as i64,
    );
    let _ = coll
        .delete_many(mongodb::bson::doc! { "seen_at": { "$lt": cutoff } })
        .await;

    let now = mongodb::bson::DateTime::now();
    let res = coll
        .insert_one(mongodb::bson::doc! {
            "event_id": event_id,
            "seen_at": now,
        })
        .await;
    match res {
        Ok(_) => true,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("E11000") || msg.contains("duplicate key") {
                false
            } else {
                tracing::warn!("event dedup insert failed: {e}");
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
struct MintResp {
    code: String,
    expires_in_secs: u64,
}

pub async fn handle_mint_link_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<MintQuery>,
) -> Response {
    if state.slack.is_none() {
        return (StatusCode::NOT_FOUND, "slack not configured").into_response();
    }

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

    Json(MintResp {
        code,
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

/// Ensure MongoDB indexes used by Slack flows.
pub async fn ensure_indexes(db: &mongodb::Database) -> Result<(), String> {
    use mongodb::bson::doc;
    use mongodb::{options::IndexOptions, IndexModel};

    let seen: mongodb::Collection<mongodb::bson::Document> = db.collection(SEEN_EVENTS_COLLECTION);
    let seen_idx = IndexModel::builder()
        .keys(doc! { "event_id": 1 })
        .options(IndexOptions::builder().unique(true).build())
        .build();
    seen.create_index(seen_idx)
        .await
        .map_err(|e| format!("seen idx: {e}"))?;

    let channels: mongodb::Collection<mongodb::bson::Document> =
        db.collection(CHANNELS_COLLECTION);
    let channels_idx = IndexModel::builder()
        .keys(doc! { "channel": 1 })
        .options(IndexOptions::builder().unique(true).build())
        .build();
    channels
        .create_index(channels_idx)
        .await
        .map_err(|e| format!("channels idx: {e}"))?;

    let codes: mongodb::Collection<mongodb::bson::Document> =
        db.collection(LINK_CODES_COLLECTION);
    let codes_idx = IndexModel::builder()
        .keys(doc! { "code": 1 })
        .options(IndexOptions::builder().unique(true).build())
        .build();
    codes
        .create_index(codes_idx)
        .await
        .map_err(|e| format!("codes idx: {e}"))?;

    Ok(())
}

// ─── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_signature_valid() {
        let secret = "8f742231b10e8888abcd99yyyzzz85a5";
        let timestamp = &chrono::Utc::now().timestamp().to_string();
        let body = b"token=xyzz0WbapA4vBCDEFasx0q6G&team_id=T1DC2JH3J&api_app_id=A015QFHBT8P";

        let sig_basestring = format!("v0:{timestamp}:{}", String::from_utf8_lossy(body));
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(sig_basestring.as_bytes());
        let expected = format!("v0={}", hex::encode(mac.finalize().into_bytes()));

        assert!(verify_slack_signature(secret, timestamp, body, &expected));
    }

    #[test]
    fn verify_signature_bad() {
        let secret = "8f742231b10e8888abcd99yyyzzz85a5";
        let timestamp = &chrono::Utc::now().timestamp().to_string();
        let body = b"hello";
        assert!(!verify_slack_signature(secret, timestamp, body, "v0=bad"));
    }

    #[test]
    fn verify_signature_replay() {
        let secret = "test";
        let old_timestamp = "1000000000"; // way in the past
        let body = b"hello";
        let sig_basestring = format!("v0:{old_timestamp}:{}", String::from_utf8_lossy(body));
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(sig_basestring.as_bytes());
        let sig = format!("v0={}", hex::encode(mac.finalize().into_bytes()));
        assert!(!verify_slack_signature(secret, old_timestamp, body, &sig));
    }

    #[test]
    fn chunk_short_is_single() {
        let out = chunk_for_slack("short message");
        assert_eq!(out, vec!["short message".to_string()]);
    }

    #[test]
    fn chunk_splits_on_paragraph() {
        let para = "a".repeat(SLACK_MSG_LIMIT - 100);
        let text = format!("{para}\n\n{para}");
        let chunks = chunk_for_slack(&text);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].len() <= SLACK_MSG_LIMIT);
        assert!(chunks[1].len() <= SLACK_MSG_LIMIT);
    }

    #[test]
    fn chunk_hard_splits_oversize_paragraph() {
        let giant = "x".repeat(SLACK_MSG_LIMIT * 2 + 200);
        let chunks = chunk_for_slack(&giant);
        assert!(chunks.len() >= 3);
        for c in &chunks {
            assert!(c.len() <= SLACK_MSG_LIMIT);
        }
    }

    #[test]
    fn strip_mention_basic() {
        assert_eq!(strip_mention_prefix("<@U12345> sessions"), "sessions");
        assert_eq!(strip_mention_prefix("<@U12345>sessions"), "sessions");
        assert_eq!(strip_mention_prefix("sessions"), "sessions");
        assert_eq!(strip_mention_prefix("  <@U12345>  here foo  "), "here foo");
    }

    #[test]
    fn resolve_exact_id() {
        let s = vec![mk_session("abc123", "repl", Some("vizsla"))];
        match resolve_session_target(&s, "abc123") {
            ResolveTarget::Exact(id) => assert_eq!(id, "abc123"),
            _ => panic!("expected Exact"),
        }
    }

    #[test]
    fn resolve_exact_nickname() {
        let s = vec![mk_session("abc123", "repl", Some("vizsla"))];
        match resolve_session_target(&s, "vizsla") {
            ResolveTarget::Exact(id) => assert_eq!(id, "abc123"),
            _ => panic!("expected Exact"),
        }
    }

    #[test]
    fn resolve_none() {
        let s = vec![mk_session("abc123", "repl", Some("vizsla"))];
        assert!(matches!(
            resolve_session_target(&s, "nope"),
            ResolveTarget::None
        ));
    }

    fn mk_session(
        id: &str,
        name: &str,
        nickname: Option<&str>,
    ) -> crate::types::SessionInfo {
        crate::types::SessionInfo {
            id: id.to_string(),
            name: name.to_string(),
            nickname: nickname.map(|s| s.to_string()),
            cwd: "/tmp".to_string(),
            agent_type: String::new(),
            hostname: String::new(),
            owner_origin: None,
            connected_at: chrono::Utc::now(),
            viewers: 0,
        }
    }
}
