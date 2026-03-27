use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use crate::types::SessionInfo;

const REPLAY_BUFFER_SIZE: usize = 8 * 1024;
const HEARTBEAT_INTERVAL_SECS: u64 = 30;
const SESSION_TTL_SECS: i64 = 90; // sessions expire if no heartbeat for 90s

/// Message sent to the tunnel WebSocket from viewers or peer bus relay.
pub enum TunnelMsg {
    Data(Vec<u8>),
    /// Multiplex bus JSON (WebSocket text frame).
    Text(String),
}

/// A connected viewer.
pub struct ViewerHandle {
    pub id: String,
    pub tx: mpsc::UnboundedSender<Vec<u8>>,
}

/// Live connection state for a session (in-memory only).
pub struct LiveSession {
    pub session_id: String,
    pub user_id: String,
    /// When true, tunnel may send/receive `ch: "bus"` on text frames.
    pub multiplex: bool,
    pub tunnel_tx: mpsc::UnboundedSender<TunnelMsg>,
    pub viewers: Arc<RwLock<Vec<ViewerHandle>>>,
    pub replay_buffer: Arc<RwLock<ReplayBuffer>>,
}

/// A simple ring buffer that keeps the last N bytes.
pub struct ReplayBuffer {
    buf: VecDeque<u8>,
    capacity: usize,
}

impl ReplayBuffer {
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
/// in-memory HashMap for live WebSocket state (tunnel_tx, viewers, replay).
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
            replay_buffer: Arc::new(RwLock::new(ReplayBuffer::new(REPLAY_BUFFER_SIZE))),
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
        mpsc::UnboundedReceiver<Vec<u8>>,
        mpsc::UnboundedSender<TunnelMsg>,
        String,
    )> {
        let live = self.live.read().await;
        let session = live.get(session_id)?;

        if session.user_id != user_id {
            return None;
        }

        let replay = session.replay_buffer.read().await.snapshot();
        let tunnel_tx = session.tunnel_tx.clone();
        let viewer_id = uuid::Uuid::new_v4().to_string();

        let (tx, rx) = mpsc::unbounded_channel();
        session.viewers.write().await.push(ViewerHandle {
            id: viewer_id.clone(),
            tx,
        });

        Some((replay, rx, tunnel_tx, viewer_id))
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

    /// Snapshot of the current replay ring buffer (for on-demand history in the web terminal).
    pub async fn replay_snapshot(&self, session_id: &str) -> Vec<u8> {
        let live = self.live.read().await;
        if let Some(session) = live.get(session_id) {
            session.replay_buffer.read().await.snapshot()
        } else {
            Vec::new()
        }
    }

    /// Broadcast data from tunnel to all viewers and append to replay buffer.
    pub async fn broadcast_to_viewers(&self, session_id: &str, data: &[u8]) {
        let live = self.live.read().await;
        if let Some(session) = live.get(session_id) {
            session.replay_buffer.write().await.push(data);
            let viewers = session.viewers.read().await;
            for viewer in viewers.iter() {
                let _ = viewer.tx.send(data.to_vec());
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
    pub async fn forward_bus_json_to_user_multiplex(&self, user_id: &str, text: &str) {
        let live = self.live.read().await;
        for (_sid, sess) in live.iter() {
            if sess.user_id != user_id {
                continue;
            }
            if !sess.multiplex {
                continue;
            }
            let _ = sess.tunnel_tx.send(TunnelMsg::Text(text.to_string()));
        }
    }
}
