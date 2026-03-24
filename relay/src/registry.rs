use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use crate::types::SessionInfo;

const REPLAY_BUFFER_SIZE: usize = 50 * 1024; // 50KB

/// Message sent to the tunnel WebSocket from viewers/relay.
pub enum TunnelMsg {
    /// Raw PTY input bytes (from viewer keyboard).
    Data(Vec<u8>),
    /// JSON control message (viewer_connected, resize, etc.) — sent as Text frame.
    Control(String),
}

/// A connected viewer.
pub struct ViewerHandle {
    pub id: String,
    pub tx: mpsc::UnboundedSender<Vec<u8>>,
}

/// A single tunnel session with its replay buffer and viewer list.
pub struct Session {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub agent_type: String,
    pub cwd: String,
    pub hostname: String,
    pub connected_at: chrono::DateTime<chrono::Utc>,
    /// Send data back to the tunnel.
    pub tunnel_tx: mpsc::UnboundedSender<TunnelMsg>,
    /// Connected viewers.
    pub viewers: Arc<RwLock<Vec<ViewerHandle>>>,
    /// Ring buffer of recent PTY output for replay on viewer connect.
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

/// In-memory session registry. Clone-safe via internal Arc.
#[derive(Clone)]
pub struct Registry {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    user_sessions: Arc<RwLock<HashMap<String, Vec<String>>>>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            user_sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a new tunnel session. Returns the session_id.
    pub async fn register(
        &self,
        user_id: String,
        name: String,
        agent_type: String,
        cwd: String,
        hostname: String,
        tunnel_tx: mpsc::UnboundedSender<TunnelMsg>,
    ) -> String {
        let session_id = uuid::Uuid::new_v4().to_string();
        let session = Session {
            id: session_id.clone(),
            user_id: user_id.clone(),
            name,
            agent_type,
            cwd,
            hostname,
            connected_at: chrono::Utc::now(),
            tunnel_tx,
            viewers: Arc::new(RwLock::new(Vec::new())),
            replay_buffer: Arc::new(RwLock::new(ReplayBuffer::new(REPLAY_BUFFER_SIZE))),
        };

        self.sessions.write().await.insert(session_id.clone(), session);
        self.user_sessions
            .write()
            .await
            .entry(user_id)
            .or_default()
            .push(session_id.clone());

        session_id
    }

    /// Unregister a session (tunnel disconnected).
    pub async fn unregister(&self, session_id: &str) {
        let session = self.sessions.write().await.remove(session_id);
        if let Some(session) = session {
            let mut user_map = self.user_sessions.write().await;
            if let Some(ids) = user_map.get_mut(&session.user_id) {
                ids.retain(|id| id != session_id);
                if ids.is_empty() {
                    user_map.remove(&session.user_id);
                }
            }
        }
    }

    /// Get all sessions for a user.
    pub async fn get_sessions(&self, user_id: &str) -> Vec<SessionInfo> {
        let sessions = self.sessions.read().await;
        let user_sessions = self.user_sessions.read().await;

        let Some(session_ids) = user_sessions.get(user_id) else {
            return Vec::new();
        };

        let mut result = Vec::new();
        for sid in session_ids {
            if let Some(s) = sessions.get(sid) {
                let viewer_count = s.viewers.read().await.len();
                result.push(SessionInfo {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    agent_type: s.agent_type.clone(),
                    cwd: s.cwd.clone(),
                    hostname: s.hostname.clone(),
                    connected_at: s.connected_at,
                    viewers: viewer_count,
                });
            }
        }
        result
    }

    /// Add a viewer to a session. Returns (replay_data, viewer_rx) or None if session not found.
    /// The viewer_rx receives data from the tunnel.
    /// Also returns the tunnel_tx so the viewer can send data back to the tunnel.
    pub async fn add_viewer(
        &self,
        session_id: &str,
        user_id: &str,
    ) -> Option<(Vec<u8>, mpsc::UnboundedReceiver<Vec<u8>>, mpsc::UnboundedSender<TunnelMsg>, String)>
    {
        let sessions = self.sessions.read().await;
        let session = sessions.get(session_id)?;

        // Verify user_id matches
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
        let sessions = self.sessions.read().await;
        if let Some(session) = sessions.get(session_id) {
            session
                .viewers
                .write()
                .await
                .retain(|v| v.id != viewer_id);
        }
    }

    /// Broadcast data from tunnel to all viewers and append to replay buffer.
    pub async fn broadcast_to_viewers(&self, session_id: &str, data: &[u8]) {
        let sessions = self.sessions.read().await;
        if let Some(session) = sessions.get(session_id) {
            // Append to replay buffer
            session.replay_buffer.write().await.push(data);

            // Send to all viewers
            let viewers = session.viewers.read().await;
            for viewer in viewers.iter() {
                let _ = viewer.tx.send(data.to_vec());
            }
        }
    }

    /// Get the viewer count for a session.
    pub async fn viewer_count(&self, session_id: &str) -> usize {
        let sessions = self.sessions.read().await;
        if let Some(session) = sessions.get(session_id) {
            session.viewers.read().await.len()
        } else {
            0
        }
    }
}
