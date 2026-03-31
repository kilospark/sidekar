use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path, Query, State, WebSocketUpgrade,
    },
    http::HeaderMap,
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::auth;
use crate::registry::{Registry, ViewerRoute};
use crate::types::{RegisterMsg, SessionInfo};

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub db: mongodb::Database,
    pub registry: Registry,
    pub jwt_secret: String,
}

// ─── Tunnel handler ───────────────────────────────────────────────

/// Upgrade a tunnel connection (sidekar CLI → relay).
pub async fn handle_tunnel_upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // Extract device token from Authorization header
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (axum::http::StatusCode::UNAUTHORIZED, "missing Authorization header")
                .into_response()
        }
    };

    // Validate device token
    let user_id = match auth::validate_device_token(&state.db, &token).await {
        Some(uid) => uid,
        None => {
            return (axum::http::StatusCode::UNAUTHORIZED, "invalid device token").into_response()
        }
    };

    ws.on_upgrade(move |socket| handle_tunnel_socket(socket, user_id, state))
}

async fn handle_tunnel_socket(socket: WebSocket, user_id: String, state: AppState) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // First message must be a register message (text frame)
    let register_msg: RegisterMsg = match ws_rx.next().await {
        Some(Ok(Message::Text(text))) => match serde_json::from_str(&text) {
            Ok(msg) => msg,
            Err(e) => {
                tracing::error!("invalid register message: {e}");
                return;
            }
        },
        _ => {
            tracing::error!("expected text register message as first frame");
            return;
        }
    };

    // Create channel for data flowing TO the tunnel (from viewers)
    let (tunnel_tx, mut tunnel_rx) = mpsc::unbounded_channel::<crate::registry::TunnelMsg>();

    let multiplex = register_msg.proto.unwrap_or(1) >= 2;

    // Register session
    let session_id = state
        .registry
        .register(
            user_id.clone(),
            register_msg.session_name,
            register_msg.agent_type,
            register_msg.cwd,
            register_msg.hostname,
            register_msg.nickname,
            multiplex,
            register_msg.cols.unwrap_or(80),
            register_msg.rows.unwrap_or(24),
            tunnel_tx,
        )
        .await;

    tracing::info!(session_id = %session_id, "tunnel registered");

    // Send registered confirmation
    let confirmation = serde_json::json!({
        "type": "registered",
        "session_id": session_id,
        "proto": if multiplex { 2 } else { 1 },
    });
    if ws_tx
        .send(Message::Text(confirmation.to_string().into()))
        .await
        .is_err()
    {
        state.registry.unregister(&session_id).await;
        return;
    }

    // Main loop: bridge tunnel ↔ viewers
    loop {
        tokio::select! {
            // Data from the tunnel WebSocket
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        // PTY data → broadcast to all viewers
                        state.registry.broadcast_to_viewers(&session_id, &data).await;
                    }
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            if v.get("ch").and_then(|x| x.as_str()) == Some("bus") {
                                let from_session =
                                    v.get("from_session").and_then(|x| x.as_str());
                                let sender = v.get("sender").and_then(|x| x.as_str());
                                let body = v.get("body").and_then(|x| x.as_str());
                                let envelope_json =
                                    v.get("envelope_json").and_then(|x| x.as_str());
                                if let (Some(sender), Some(body)) = (sender, body) {
                                    if let Some(recipient_session_id) =
                                        v.get("recipient_session_id").and_then(|x| x.as_str())
                                    {
                                        state
                                            .registry
                                            .enqueue_bus_for_session(
                                                &user_id,
                                                recipient_session_id,
                                                sender,
                                                body,
                                                envelope_json,
                                                from_session,
                                            )
                                            .await;
                                    } else if let Some(recipient) =
                                        v.get("recipient").and_then(|x| x.as_str())
                                    {
                                        state
                                            .registry
                                            .enqueue_bus_for_recipient_name(
                                                &user_id,
                                                recipient,
                                                sender,
                                                body,
                                                envelope_json,
                                                from_session,
                                            )
                                            .await;
                                    }
                                }
                                continue;
                            }
                            if v.get("ch").and_then(|x| x.as_str()) == Some("pty")
                                && v.get("event").and_then(|x| x.as_str()) == Some("resize")
                            {
                                let cols = v.get("cols").and_then(|x| x.as_u64()).unwrap_or(80) as u16;
                                let rows = v.get("rows").and_then(|x| x.as_u64()).unwrap_or(24) as u16;
                                state
                                    .registry
                                    .update_terminal_size(&session_id, cols, rows)
                                    .await;
                                continue;
                            }
                        }
                        tracing::debug!(session_id = %session_id, "tunnel text (ignored): {text}");
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        tracing::info!(session_id = %session_id, "tunnel disconnected");
                        break;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        let _ = ws_tx.send(Message::Pong(data)).await;
                    }
                    Some(Err(e)) => {
                        tracing::error!(session_id = %session_id, "tunnel error: {e}");
                        break;
                    }
                    _ => {}
                }
            }
            // Viewer keyboard input or peer bus → tunnel
            Some(msg) = tunnel_rx.recv() => {
                match msg {
                    crate::registry::TunnelMsg::Data(data) => {
                        if ws_tx.send(Message::Binary(Bytes::from(data))).await.is_err() {
                            break;
                        }
                    }
                    crate::registry::TunnelMsg::Text(text) => {
                        if ws_tx.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
    }

    // Cleanup
    state.registry.unregister(&session_id).await;
    tracing::info!(session_id = %session_id, "tunnel session cleaned up");
}

// ─── Relay bus HTTP (device token) ────────────────────────────────

#[derive(Deserialize)]
pub struct RelayBusIn {
    #[serde(default)]
    pub recipient_session_id: Option<String>,
    #[serde(default)]
    pub recipient: Option<String>,
    pub sender: String,
    pub body: String,
    #[serde(default)]
    pub envelope_json: Option<String>,
    /// If set, skip this session when forwarding (prevents self-delivery loops).
    #[serde(default)]
    pub from_session: Option<String>,
}

const MAX_BUS_BODY_BYTES: usize = 64 * 1024; // 64 KB

/// POST /relay/bus — deliver a bus message to all multiplex tunnels for this user.
pub async fn handle_relay_bus(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RelayBusIn>,
) -> Response {
    if body.body.len() > MAX_BUS_BODY_BYTES {
        return (
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            "body exceeds 64KB limit",
        )
            .into_response();
    }

    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (axum::http::StatusCode::UNAUTHORIZED, "missing Authorization header")
                .into_response()
        }
    };

    let user_id = match auth::validate_device_token(&state.db, &token).await {
        Some(uid) => uid,
        None => {
            return (axum::http::StatusCode::UNAUTHORIZED, "invalid device token").into_response()
        }
    };

    if let Some(recipient_session_id) = body.recipient_session_id.as_deref() {
        state
            .registry
            .enqueue_bus_for_session(
                &user_id,
                recipient_session_id,
                &body.sender,
                &body.body,
                body.envelope_json.as_deref(),
                body.from_session.as_deref(),
            )
            .await;
    } else if let Some(recipient) = body.recipient.as_deref() {
        state
            .registry
            .enqueue_bus_for_recipient_name(
                &user_id,
                recipient,
                &body.sender,
                &body.body,
                body.envelope_json.as_deref(),
                body.from_session.as_deref(),
            )
            .await;
    } else {
        return (axum::http::StatusCode::BAD_REQUEST, "missing recipient").into_response();
    }

    Json(serde_json::json!({ "ok": true })).into_response()
}

// ─── Viewer handler ───────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ViewerQuery {
    pub token: Option<String>,
}

/// Upgrade a viewer connection (browser → relay).
pub async fn handle_viewer_upgrade(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Query(query): Query<ViewerQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // Extract JWT from query param or cookie
    let jwt = query
        .token
        .or_else(|| extract_cookie_token(&headers, "sidekar_session"));

    let jwt = match jwt {
        Some(t) => t,
        None => {
            return (axum::http::StatusCode::UNAUTHORIZED, "missing token").into_response()
        }
    };

    let user_id = match auth::validate_session_jwt(&jwt, &state.jwt_secret) {
        Some(uid) => uid,
        None => {
            return (axum::http::StatusCode::UNAUTHORIZED, "invalid token").into_response()
        }
    };

    match state.registry.resolve_viewer_route(&session_id, &user_id).await {
        Some(ViewerRoute::Local) => {}
        Some(ViewerRoute::Remote { owner_origin }) => {
            return (
                axum::http::StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "session owned by another relay instance",
                    "owner_origin": owner_origin,
                })),
            )
                .into_response();
        }
        None => {
            return (axum::http::StatusCode::NOT_FOUND, "session not found").into_response();
        }
    }

    ws.on_upgrade(move |socket| handle_viewer_socket(socket, session_id, user_id, state))
}

#[derive(Deserialize)]
pub struct ResolveQuery {
    pub token: Option<String>,
}

/// GET /session/{id}/resolve — resolve the current owner origin for this session.
pub async fn handle_resolve_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Query(query): Query<ResolveQuery>,
    headers: HeaderMap,
) -> Response {
    let jwt = query
        .token
        .or_else(|| extract_cookie_token(&headers, "sidekar_session"));

    let jwt = match jwt {
        Some(t) => t,
        None => return (axum::http::StatusCode::UNAUTHORIZED, "missing token").into_response(),
    };

    let user_id = match auth::validate_session_jwt(&jwt, &state.jwt_secret) {
        Some(uid) => uid,
        None => return (axum::http::StatusCode::UNAUTHORIZED, "invalid token").into_response(),
    };

    match state.registry.resolve_viewer_route(&session_id, &user_id).await {
        Some(ViewerRoute::Local) => Json(serde_json::json!({
            "session_id": session_id,
            "owner_origin": state.registry.public_origin(),
        }))
        .into_response(),
        Some(ViewerRoute::Remote { owner_origin }) => Json(serde_json::json!({
            "session_id": session_id,
            "owner_origin": owner_origin,
        }))
        .into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, "session not found").into_response(),
    }
}

async fn handle_viewer_socket(
    socket: WebSocket,
    session_id: String,
    user_id: String,
    state: AppState,
) {
    // Add viewer to session
    let (scrollback, terminal_size, mut viewer_rx, tunnel_tx, viewer_id) =
        match state.registry.add_viewer(&session_id, &user_id).await {
            Some(v) => v,
            None => {
                tracing::warn!(session_id = %session_id, "viewer rejected: session not found or user mismatch");
                return;
            }
        };

    tracing::info!(session_id = %session_id, viewer_id = %viewer_id, "viewer connected");

    let (mut ws_tx, mut ws_rx) = socket.split();

    // Protocol v1: send a short session hello, then the current scrollback snapshot once.
    let scrollback_len = scrollback.len();
    let session_hello = serde_json::json!({
        "type": "session",
        "v": 1,
        "scrollback_bytes": scrollback_len,
        "cols": terminal_size.cols,
        "rows": terminal_size.rows,
    });
    if ws_tx
        .send(Message::Text(session_hello.to_string().into()))
        .await
        .is_err()
    {
        state
            .registry
            .remove_viewer(&session_id, &viewer_id)
            .await;
        return;
    }
    if scrollback_len > 0 {
        if ws_tx
            .send(Message::Binary(Bytes::from(scrollback)))
            .await
            .is_err()
        {
            state
                .registry
                .remove_viewer(&session_id, &viewer_id)
                .await;
            return;
        }
    }

    // Main loop
    loop {
        tokio::select! {
            // Data from tunnel → viewer
            Some(msg) = viewer_rx.recv() => {
                match msg {
                    crate::registry::ViewerMsg::Data(data) => {
                        if ws_tx.send(Message::Binary(Bytes::from(data))).await.is_err() {
                            break;
                        }
                    }
                    crate::registry::ViewerMsg::Control(text) => {
                        if ws_tx.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
            // Data from viewer → tunnel
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        let _ = tunnel_tx.send(crate::registry::TunnelMsg::Data(data.to_vec()));
                    }
                    Some(Ok(Message::Text(_text))) => {}
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        let _ = ws_tx.send(Message::Pong(data)).await;
                    }
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }

    // Cleanup
    state
        .registry
        .remove_viewer(&session_id, &viewer_id)
        .await;

    tracing::info!(session_id = %session_id, viewer_id = %viewer_id, "viewer disconnected");
}

// ─── Session list handler ─────────────────────────────────────────

/// GET /sessions — list active sessions for the authenticated user.
pub async fn handle_list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ViewerQuery>,
) -> Response {
    let user_id = if let Some(bearer) = extract_bearer_token(&headers) {
        match auth::validate_device_token(&state.db, &bearer).await {
            Some(uid) => uid,
            None => {
                return (axum::http::StatusCode::UNAUTHORIZED, "invalid device token").into_response()
            }
        }
    } else {
        let jwt = query
            .token
            .or_else(|| extract_cookie_token(&headers, "sidekar_session"));

        let jwt = match jwt {
            Some(t) => t,
            None => {
                return (axum::http::StatusCode::UNAUTHORIZED, "missing token").into_response()
            }
        };

        match auth::validate_session_jwt(&jwt, &state.jwt_secret) {
            Some(uid) => uid,
            None => {
                return (axum::http::StatusCode::UNAUTHORIZED, "invalid token").into_response()
            }
        }
    };

    let sessions: Vec<SessionInfo> = state.registry.get_sessions(&user_id).await;
    Json(serde_json::json!({ "sessions": sessions })).into_response()
}

// ─── Helpers ──────────────────────────────────────────────────────

fn extract_bearer_token(headers: &HeaderMap) -> Option<String> {
    let auth = headers.get("authorization")?.to_str().ok()?;
    let token = auth.strip_prefix("Bearer ")?;
    Some(token.to_string())
}

fn extract_cookie_token(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookie_header = headers.get("cookie")?.to_str().ok()?;
    for part in cookie_header.split(';') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix(&format!("{name}=")) {
            return Some(value.to_string());
        }
    }
    None
}
