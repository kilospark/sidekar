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
use crate::registry::Registry;
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

    // Register session
    let session_id = state
        .registry
        .register(
            user_id,
            register_msg.session_name,
            register_msg.agent_type,
            register_msg.cwd,
            register_msg.hostname,
            register_msg.nickname,
            tunnel_tx,
        )
        .await;

    tracing::info!(session_id = %session_id, "tunnel registered");

    // Send registered confirmation
    let confirmation = serde_json::json!({
        "type": "registered",
        "session_id": session_id,
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
                        // Control message (e.g., resize) — could forward to viewers
                        tracing::debug!(session_id = %session_id, "tunnel control: {text}");
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
            // Viewer keyboard input → send to tunnel as binary
            Some(crate::registry::TunnelMsg::Data(data)) = tunnel_rx.recv() => {
                if ws_tx.send(Message::Binary(Bytes::from(data))).await.is_err() {
                    break;
                }
            }
        }
    }

    // Cleanup
    state.registry.unregister(&session_id).await;
    tracing::info!(session_id = %session_id, "tunnel session cleaned up");
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

    ws.on_upgrade(move |socket| handle_viewer_socket(socket, session_id, user_id, state))
}

async fn handle_viewer_socket(
    socket: WebSocket,
    session_id: String,
    user_id: String,
    state: AppState,
) {
    // Add viewer to session
    let (replay, mut viewer_rx, tunnel_tx, viewer_id) =
        match state.registry.add_viewer(&session_id, &user_id).await {
            Some(v) => v,
            None => {
                tracing::warn!(session_id = %session_id, "viewer rejected: session not found or user mismatch");
                return;
            }
        };

    tracing::info!(session_id = %session_id, viewer_id = %viewer_id, "viewer connected");

    let (mut ws_tx, mut ws_rx) = socket.split();

    // Protocol v1: first frame is JSON so the browser can defer replay until the user scrolls up.
    let replay_len = replay.len();
    let session_hello = serde_json::json!({
        "type": "session",
        "v": 1,
        "replay_len": replay_len,
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
    if replay_len > 0 {
        if ws_tx
            .send(Message::Binary(Bytes::from(replay)))
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
            Some(data) = viewer_rx.recv() => {
                if ws_tx.send(Message::Binary(Bytes::from(data))).await.is_err() {
                    break;
                }
            }
            // Data from viewer → tunnel
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        let _ = tunnel_tx.send(crate::registry::TunnelMsg::Data(data.to_vec()));
                    }
                    Some(Ok(Message::Text(text))) => {
                        // Web terminal: fetch relay replay buffer (PTY scrollback is not available remotely).
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            if v.get("type").and_then(|t| t.as_str()) == Some("history")
                                && v.get("v").and_then(|x| x.as_u64()) == Some(1)
                            {
                                let snap = state.registry.replay_snapshot(&session_id).await;
                                if snap.is_empty() {
                                    let ack = serde_json::json!({
                                        "type": "history",
                                        "v": 1,
                                        "empty": true,
                                    });
                                    let _ = ws_tx
                                        .send(Message::Text(ack.to_string().into()))
                                        .await;
                                } else {
                                    let n = snap.len();
                                    let hdr = serde_json::json!({
                                        "type": "history",
                                        "v": 1,
                                        "empty": false,
                                        "bytes": n,
                                    });
                                    if ws_tx
                                        .send(Message::Text(hdr.to_string().into()))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                    let _ = ws_tx.send(Message::Binary(Bytes::from(snap))).await;
                                }
                                continue;
                            }
                        }
                    }
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
