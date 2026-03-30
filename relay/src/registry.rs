use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use crate::types::SessionInfo;

const SCROLLBACK_BUFFER_SIZE: usize = 512 * 1024;
const HEARTBEAT_INTERVAL_SECS: u64 = 30;
const SESSION_TTL_SECS: i64 = 90; // sessions expire if no heartbeat for 90s

#[derive(Clone, Copy)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

/// Message sent to the tunnel WebSocket from viewers or peer bus relay.
pub enum TunnelMsg {
    Data(Vec<u8>),
    /// Multiplex bus JSON (WebSocket text frame).
    Text(String),
}

/// A connected viewer.
pub struct ViewerHandle {
    pub id: String,
    pub tx: mpsc::UnboundedSender<ViewerMsg>,
}

pub enum ViewerMsg {
    Data(Vec<u8>),
    Control(String),
}

/// Live connection state for a session (in-memory only).
pub struct LiveSession {
    pub session_id: String,
    pub user_id: String,
    /// When true, tunnel may send/receive `ch: "bus"` on text frames.
    pub multiplex: bool,
    pub tunnel_tx: mpsc::UnboundedSender<TunnelMsg>,
    pub viewers: Arc<RwLock<Vec<ViewerHandle>>>,
    pub scrollback_buffer: Arc<RwLock<ScrollbackBuffer>>,
    pub terminal_size: Arc<RwLock<TerminalSize>>,
}

/// A simple ring buffer that keeps the most recent PTY output bytes for web scrollback.
pub struct ScrollbackBuffer {
    buf: VecDeque<u8>,
    capacity: usize,
}

impl ScrollbackBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, data: &[u8]) {
        for &byte in data {
            if self.buf.len() == self.capacity {
                self.buf.pop_front();
            }
            self.buf.push_back(byte);
        }
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.buf.iter().copied().collect()
    }
}

/// Hybrid registry: MongoDB for session metadata (discovery/listing),
/// in-memory HashMap for live WebSocket state (tunnel_tx, viewers, scrollback).
#[derive(Clone)]
pub struct Registry {
    db: mongodb::Database,
    /// In-memory map: session_id → live connection state.
    live: Arc<RwLock<HashMap<String, LiveSession>>>,
}

impl Registry {
    pub fn new(db: mongodb::Database) -> Self {
        Self {
            db,
            live: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Start the background heartbeat task that keeps our sessions alive in MongoDB.
    pub fn start_heartbeat(&self) {
        let db = self.db.clone();
        let live = self.live.clone();
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
            loop {
                interval.tick().await;
                let session_ids: Vec<String> = {
                    live.read().await.keys().cloned().collect()
                };
                if session_ids.is_empty() {
                    continue;
                }
                let now = mongodb::bson::DateTime::now();
                let _ = db
                    .collection::<mongodb::bson::Document>("sessions")
                    .update_many(
                        mongodb::bson::doc! {
                            "session_id": { "$in": &session_ids }
                        },
                        mongodb::bson::doc! {
                            "$set": { "last_heartbeat": now }
                        },
                    )
                    .await;
            }
        });
    }

    /// Register a new tunnel session. Writes metadata to MongoDB, stores
    /// live connection state in-memory. Returns the session_id.
    pub async fn register(
        &self,
        user_id: String,
        name: String,
        agent_type: String,
        cwd: String,
        hostname: String,
        nickname: Option<String>,
        multiplex: bool,
        cols: u16,
        rows: u16,
        tunnel_tx: mpsc::UnboundedSender<TunnelMsg>,
    ) -> String {
        let session_id = uuid::Uuid::new_v4().to_string();
        let now = mongodb::bson::DateTime::now();

        // Write metadata to MongoDB
        let doc = mongodb::bson::doc! {
            "session_id": &session_id,
            "user_id": &user_id,
            "name": &name,
            "agent_type": &agent_type,
            "cwd": &cwd,
            "hostname": &hostname,
            "nickname": &nickname,
            "connected_at": now,
            "last_heartbeat": now,
        };
        let _ = self
            .db
            .collection::<mongodb::bson::Document>("sessions")
            .insert_one(doc)
            .await;

        // Store live state in-memory
        let live_session = LiveSession {
            session_id: session_id.clone(),
            user_id,
            multiplex,
            tunnel_tx,
            viewers: Arc::new(RwLock::new(Vec::new())),
            scrollback_buffer: Arc::new(RwLock::new(ScrollbackBuffer::new(SCROLLBACK_BUFFER_SIZE))),
            terminal_size: Arc::new(RwLock::new(TerminalSize { cols, rows })),
        };
        self.live
            .write()
            .await
            .insert(session_id.clone(), live_session);

        session_id
    }

    /// Unregister a session (tunnel disconnected). Removes from both MongoDB and memory.
    pub async fn unregister(&self, session_id: &str) {
        self.live.write().await.remove(session_id);
        let _ = self
            .db
            .collection::<mongodb::bson::Document>("sessions")
            .delete_one(mongodb::bson::doc! { "session_id": session_id })
            .await;
    }

    /// Get all sessions for a user. Queries MongoDB so it works across relay instances.
    pub async fn get_sessions(&self, user_id: &str) -> Vec<SessionInfo> {
        use futures_util::StreamExt;

        // Only return sessions with a recent heartbeat (not orphaned)
        let cutoff = mongodb::bson::DateTime::from_millis(
            chrono::Utc::now().timestamp_millis() - (SESSION_TTL_SECS * 1000),
        );

        let filter = mongodb::bson::doc! {
            "user_id": user_id,
            "last_heartbeat": { "$gt": cutoff },
        };

        let mut cursor = match self
            .db
            .collection::<mongodb::bson::Document>("sessions")
            .find(filter)
            .await
        {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let live = self.live.read().await;
        let mut result = Vec::new();

        while let Some(Ok(doc)) = cursor.next().await {
            let sid = doc.get_str("session_id").unwrap_or_default().to_string();
            let viewer_count = if let Some(ls) = live.get(&sid) {
                ls.viewers.read().await.len()
            } else {
                0
            };

            let connected_at = doc
                .get_datetime("connected_at")
                .ok()
                .map(|dt| {
                    chrono::DateTime::from_timestamp_millis(dt.timestamp_millis())
                        .unwrap_or_default()
                })
                .unwrap_or_default();

            result.push(SessionInfo {
                id: sid,
                name: doc.get_str("name").unwrap_or_default().to_string(),
                agent_type: doc.get_str("agent_type").unwrap_or_default().to_string(),
                cwd: doc.get_str("cwd").unwrap_or_default().to_string(),
                hostname: doc.get_str("hostname").unwrap_or_default().to_string(),
                nickname: doc.get_str("nickname").ok().map(|s| s.to_string()),
                connected_at,
                viewers: viewer_count,
            });
        }

        result
    }

    /// Add a viewer to a session. Uses in-memory state (needs the WebSocket handles).
    pub async fn add_viewer(
        &self,
        session_id: &str,
        user_id: &str,
    ) -> Option<(
        Vec<u8>,
        TerminalSize,
        mpsc::UnboundedReceiver<ViewerMsg>,
        mpsc::UnboundedSender<TunnelMsg>,
        String,
    )> {
        let live = self.live.read().await;
        let session = live.get(session_id)?;

        if session.user_id != user_id {
            return None;
        }

        let viewer_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = mpsc::unbounded_channel();

        // Keep the scrollback snapshot and viewer registration contiguous so the
        // viewer sees a continuous tail of PTY output with no attach-time gap.
        let scrollback_guard = session.scrollback_buffer.read().await;
        let terminal_size = *session.terminal_size.read().await;
        let mut viewers = session.viewers.write().await;
        let scrollback = scrollback_guard.snapshot();
        viewers.push(ViewerHandle {
            id: viewer_id.clone(),
            tx,
        });
        drop(viewers);
        drop(scrollback_guard);

        let tunnel_tx = session.tunnel_tx.clone();
        Some((scrollback, terminal_size, rx, tunnel_tx, viewer_id))
    }

    /// Remove a viewer from a session.
    pub async fn remove_viewer(&self, session_id: &str, viewer_id: &str) {
        let live = self.live.read().await;
        if let Some(session) = live.get(session_id) {
            session
                .viewers
                .write()
                .await
                .retain(|v| v.id != viewer_id);
        }
    }

    /// Broadcast data from tunnel to all viewers and append to scrollback.
    pub async fn broadcast_to_viewers(&self, session_id: &str, data: &[u8]) {
        let live = self.live.read().await;
        if let Some(session) = live.get(session_id) {
            session.scrollback_buffer.write().await.push(data);
            let viewers = session.viewers.read().await;
            for viewer in viewers.iter() {
                let _ = viewer.tx.send(ViewerMsg::Data(data.to_vec()));
            }
        }
    }

    pub async fn update_terminal_size(&self, session_id: &str, cols: u16, rows: u16) {
        let live = self.live.read().await;
        if let Some(session) = live.get(session_id) {
            *session.terminal_size.write().await = TerminalSize { cols, rows };
            let msg = serde_json::json!({
                "type": "pty",
                "v": 1,
                "event": "resize",
                "cols": cols,
                "rows": rows,
            })
            .to_string();
            let viewers = session.viewers.read().await;
            for viewer in viewers.iter() {
                let _ = viewer.tx.send(ViewerMsg::Control(msg.clone()));
            }
        }
    }

    /// Forward a bus JSON text frame to other multiplex tunnels for the same user.
    pub async fn forward_bus_to_peers(&self, from_session_id: &str, text: &str) {
        let user_id = {
            let live = self.live.read().await;
            let Some(sess) = live.get(from_session_id) else {
                return;
            };
            if !sess.multiplex {
                return;
            }
            sess.user_id.clone()
        };

        let live = self.live.read().await;
        for (sid, sess) in live.iter() {
            if sess.user_id != user_id {
                continue;
            }
            if sid == from_session_id {
                continue;
            }
            if !sess.multiplex {
                continue;
            }
            let _ = sess.tunnel_tx.send(TunnelMsg::Text(text.to_string()));
        }
    }

    /// Push a bus JSON text frame to every multiplex tunnel for this user (e.g. HTTP ingress).
    /// If `exclude_session` is provided, skip that session (prevents self-delivery loops).
    pub async fn forward_bus_json_to_user_multiplex(
        &self,
        user_id: &str,
        text: &str,
        exclude_session: Option<&str>,
    ) {
        let live = self.live.read().await;
        for (sid, sess) in live.iter() {
            if sess.user_id != user_id {
                continue;
            }
            if !sess.multiplex {
                continue;
            }
            if exclude_session == Some(sid.as_str()) {
                continue;
            }
            let _ = sess.tunnel_tx.send(TunnelMsg::Text(text.to_string()));
        }
    }
}
