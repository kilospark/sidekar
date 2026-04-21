use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use crate::types::SessionInfo;
use sha2::{Digest, Sha256};

const SCROLLBACK_BUFFER_SIZE: usize = 512 * 1024;
const HEARTBEAT_INTERVAL_SECS: u64 = 30;
const SESSION_TTL_SECS: i64 = 90; // sessions expire if no heartbeat for 90s
const BUS_DISPATCH_INTERVAL_MS: u64 = 250;
const BUS_MESSAGE_TTL_SECS: i64 = 300;
const SESSIONS_COLLECTION: &str = "sessions";
const BUS_MESSAGES_COLLECTION: &str = "relay_bus_messages";

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
    pub user_id: String,
    pub name: String,
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
    instance_id: String,
    public_origin: String,
    /// In-memory map: session_id → live connection state.
    live: Arc<RwLock<HashMap<String, LiveSession>>>,
}

impl Registry {
    pub fn public_origin(&self) -> &str {
        &self.public_origin
    }

    pub fn new(db: mongodb::Database, instance_id: String, public_origin: String) -> Self {
        Self {
            db,
            instance_id,
            public_origin,
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
                    .collection::<mongodb::bson::Document>(SESSIONS_COLLECTION)
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

    /// Start the background dispatcher that drains Mongo-backed bus messages
    /// into the locally connected multiplex sessions owned by this relay instance.
    pub fn start_bus_dispatcher(&self) {
        let db = self.db.clone();
        let live = self.live.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(
                BUS_DISPATCH_INTERVAL_MS,
            ));
            loop {
                interval.tick().await;

                let session_map: HashMap<String, (String, mpsc::UnboundedSender<TunnelMsg>)> = {
                    let live = live.read().await;
                    live.iter()
                        .filter(|(_, sess)| sess.multiplex)
                        .map(|(sid, sess)| {
                            (
                                sid.clone(),
                                (sess.name.clone(), sess.tunnel_tx.clone()),
                            )
                        })
                        .collect()
                };

                if session_map.is_empty() {
                    continue;
                }

                let session_ids: Vec<String> = session_map.keys().cloned().collect();
                let cutoff = mongodb::bson::DateTime::from_millis(
                    chrono::Utc::now().timestamp_millis() - (BUS_MESSAGE_TTL_SECS * 1000),
                );

                let collection = db.collection::<mongodb::bson::Document>(BUS_MESSAGES_COLLECTION);
                let filter = mongodb::bson::doc! {
                    "recipient_session_id": { "$in": &session_ids },
                    "created_at": { "$gt": cutoff },
                };

                let mut cursor = match collection.find(filter).await {
                    Ok(cursor) => cursor,
                    Err(_) => continue,
                };

                use futures_util::StreamExt;
                while let Some(Ok(doc)) = cursor.next().await {
                    let Some(id) = doc.get_object_id("_id").ok() else {
                        continue;
                    };
                    let Some(recipient_session_id) =
                        doc.get_str("recipient_session_id").ok().map(str::to_string)
                    else {
                        let _ = collection.delete_one(mongodb::bson::doc! { "_id": id }).await;
                        continue;
                    };
                    let Some((recipient_name, tunnel_tx)) =
                        session_map.get(&recipient_session_id).cloned()
                    else {
                        continue;
                    };
                    let sender = doc
                        .get_str("sender")
                        .ok()
                        .unwrap_or("sidekar")
                        .to_string();
                    let body = doc.get_str("body").ok().unwrap_or("").to_string();
                    let envelope_json = doc
                        .get_str("envelope_json")
                        .ok()
                        .map(str::to_string);
                    let payload = serde_json::json!({
                        "ch": "bus",
                        "v": 1,
                        "recipient": recipient_name,
                        "sender": sender,
                        "body": body,
                        "envelope_json": envelope_json,
                    })
                    .to_string();

                    if tunnel_tx.send(TunnelMsg::Text(payload)).is_ok() {
                        let _ = collection.delete_one(mongodb::bson::doc! { "_id": id }).await;
                    }
                }

                let _ = collection
                    .delete_many(mongodb::bson::doc! {
                        "created_at": { "$lte": cutoff }
                    })
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
        let session_id = stable_session_id(&user_id, &name, &hostname);
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
            "owner_instance_id": &self.instance_id,
            "owner_origin": &self.public_origin,
            "connected_at": now,
            "last_heartbeat": now,
        };
        let _ = self
            .db
            .collection::<mongodb::bson::Document>(SESSIONS_COLLECTION)
            .replace_one(
                mongodb::bson::doc! { "session_id": &session_id },
                doc,
            )
            .upsert(true)
            .await;

        // Store live state in-memory
        let live_session = LiveSession {
            user_id,
            name,
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
            .collection::<mongodb::bson::Document>(SESSIONS_COLLECTION)
            .delete_one(mongodb::bson::doc! { "session_id": session_id })
            .await;
        let _ = self
            .db
            .collection::<mongodb::bson::Document>(BUS_MESSAGES_COLLECTION)
            .delete_many(mongodb::bson::doc! { "recipient_session_id": session_id })
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
            .collection::<mongodb::bson::Document>(SESSIONS_COLLECTION)
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
                owner_origin: doc.get_str("owner_origin").ok().map(|s| s.to_string()),
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

    pub async fn resolve_viewer_route(
        &self,
        session_id: &str,
        user_id: &str,
    ) -> Option<ViewerRoute> {
        let cutoff = mongodb::bson::DateTime::from_millis(
            chrono::Utc::now().timestamp_millis() - (SESSION_TTL_SECS * 1000),
        );
        let doc = self
            .db
            .collection::<mongodb::bson::Document>(SESSIONS_COLLECTION)
            .find_one(mongodb::bson::doc! {
                "session_id": session_id,
                "user_id": user_id,
                "last_heartbeat": { "$gt": cutoff },
            })
            .await
            .ok()
            .flatten()?;

        let owner_instance_id = doc
            .get_str("owner_instance_id")
            .ok()
            .unwrap_or_default()
            .to_string();
        let owner_origin = doc
            .get_str("owner_origin")
            .ok()
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.public_origin.clone());
        let local_live = self.live.read().await.contains_key(session_id);

        if owner_instance_id.is_empty() {
            if local_live {
                Some(ViewerRoute::Local)
            } else {
                Some(ViewerRoute::Remote { owner_origin })
            }
        } else if owner_instance_id == self.instance_id {
            if local_live {
                Some(ViewerRoute::Local)
            } else {
                None
            }
        } else {
            Some(ViewerRoute::Remote { owner_origin })
        }
    }

    /// Push raw viewer-style keystroke bytes into the tunnel for a given
    /// (session_id, user_id). Returns true if delivered, false if the
    /// session is unknown locally, owned by another user, or the tunnel
    /// channel is closed.
    ///
    /// Used by non-WebSocket viewer transports (e.g. Telegram) that
    /// synthesize keystrokes on behalf of a human author.
    pub async fn push_tunnel_input(
        &self,
        session_id: &str,
        user_id: &str,
        data: Vec<u8>,
    ) -> bool {
        let live = self.live.read().await;
        let Some(session) = live.get(session_id) else {
            return false;
        };
        if session.user_id != user_id {
            return false;
        }
        session.tunnel_tx.send(TunnelMsg::Data(data)).is_ok()
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

    /// Forward a control/text frame to all viewers of a session (no scrollback).
    pub async fn broadcast_control_to_viewers(&self, session_id: &str, text: &str) {
        let live = self.live.read().await;
        if let Some(session) = live.get(session_id) {
            let viewers = session.viewers.read().await;
            for viewer in viewers.iter() {
                let _ = viewer.tx.send(ViewerMsg::Control(text.to_string()));
            }
        }
    }

    pub async fn enqueue_bus_for_session(
        &self,
        user_id: &str,
        recipient_session_id: &str,
        sender: &str,
        body: &str,
        envelope_json: Option<&str>,
        exclude_session: Option<&str>,
    ) {
        if exclude_session == Some(recipient_session_id) {
            return;
        }

        let cutoff = mongodb::bson::DateTime::from_millis(
            chrono::Utc::now().timestamp_millis() - (SESSION_TTL_SECS * 1000),
        );
        let session_exists = self
            .db
            .collection::<mongodb::bson::Document>(SESSIONS_COLLECTION)
            .find_one(mongodb::bson::doc! {
                "user_id": user_id,
                "session_id": recipient_session_id,
                "last_heartbeat": { "$gt": cutoff },
            })
            .await
            .ok()
            .flatten()
            .is_some();
        if !session_exists {
            return;
        }

        let _ = self
            .db
            .collection::<mongodb::bson::Document>(BUS_MESSAGES_COLLECTION)
            .insert_one(mongodb::bson::doc! {
                "user_id": user_id,
                "recipient_session_id": recipient_session_id,
                "sender": sender,
                "body": body,
                "envelope_json": envelope_json,
                "created_at": mongodb::bson::DateTime::now(),
            })
            .await;
    }

    pub async fn enqueue_bus_for_recipient_name(
        &self,
        user_id: &str,
        recipient_name: &str,
        sender: &str,
        body: &str,
        envelope_json: Option<&str>,
        exclude_session: Option<&str>,
    ) {
        use futures_util::StreamExt;

        let cutoff = mongodb::bson::DateTime::from_millis(
            chrono::Utc::now().timestamp_millis() - (SESSION_TTL_SECS * 1000),
        );
        let filter = mongodb::bson::doc! {
            "user_id": user_id,
            "name": recipient_name,
            "last_heartbeat": { "$gt": cutoff },
        };

        let mut cursor = match self
            .db
            .collection::<mongodb::bson::Document>(SESSIONS_COLLECTION)
            .find(filter)
            .await
        {
            Ok(cursor) => cursor,
            Err(_) => return,
        };

        while let Some(Ok(doc)) = cursor.next().await {
            if let Ok(recipient_session_id) = doc.get_str("session_id") {
                self.enqueue_bus_for_session(
                    user_id,
                    recipient_session_id,
                    sender,
                    body,
                    envelope_json,
                    exclude_session,
                )
                .await;
            }
        }
    }
}

fn stable_session_id(user_id: &str, name: &str, hostname: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(user_id.as_bytes());
    hasher.update(b":");
    hasher.update(name.as_bytes());
    hasher.update(b":");
    hasher.update(hostname.as_bytes());
    format!("relay-{}", hex::encode(hasher.finalize()))
}

pub enum ViewerRoute {
    Local,
    Remote { owner_origin: String },
}
