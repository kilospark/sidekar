//! CDP proxy — daemon-side connection pool and CLI-side proxy client.
//!
//! The daemon holds persistent WebSocket connections to Chrome's CDP,
//! keyed by ws_url. CLI commands connect to the daemon via unix socket
//! and proxy CDP requests through it, avoiding per-call WS overhead.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};

mod pool;
pub use pool::*;

const WS_PING_INTERVAL_SECS: u64 = 15;
const CONNECTION_IDLE_TIMEOUT_SECS: u64 = 120;
const MAX_EVENT_BUFFER: usize = 512;

// ---------------------------------------------------------------------------
// CLI-side: DaemonCdpProxy
// ---------------------------------------------------------------------------

/// CLI-side proxy that sends CDP commands through the daemon.
pub struct DaemonCdpProxy {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    next_req_id: u64,
    pub pending_events: VecDeque<Value>,
    pub auto_dialog: Option<(bool, String)>,
}

impl DaemonCdpProxy {
    /// Connect to the daemon and register for a specific tab.
    pub async fn connect(ws_url: &str) -> Result<Self> {
        let sock_path = crate::daemon::socket_path();
        let stream = tokio::net::UnixStream::connect(&sock_path)
            .await
            .with_context(|| format!("Cannot connect to daemon at {}", sock_path.display()))?;

        let (read_half, mut write_half) = stream.into_split();
        let reader = BufReader::new(read_half);

        // Send registration message
        let reg = json!({
            "type": "cdp_connect",
            "ws_url": ws_url,
        });
        let mut line = serde_json::to_string(&reg)?;
        line.push('\n');
        write_half.write_all(line.as_bytes()).await?;
        write_half.flush().await?;

        Ok(Self {
            reader,
            writer: write_half,
            next_req_id: 1,
            pending_events: VecDeque::new(),
            auto_dialog: None,
        })
    }

    /// Send a CDP command, optionally scoped to a session.
    async fn do_send(
        &mut self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value> {
        let req_id = self.next_req_id;
        self.next_req_id += 1;

        let mut msg = json!({
            "type": "cdp_send",
            "method": method,
            "params": params,
            "req_id": req_id,
        });
        if let Some(sid) = session_id {
            msg["session_id"] = json!(sid);
        }
        // Forward auto_dialog config so daemon can handle dialogs
        if let Some((accept, ref prompt_text)) = self.auto_dialog {
            msg["auto_dialog"] = json!({"accept": accept, "prompt_text": prompt_text});
        }

        let mut line = serde_json::to_string(&msg)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;

        let timeout_duration = super::cdp_send_timeout();
        match tokio::time::timeout(timeout_duration, self.recv_response(req_id)).await {
            Ok(result) => result,
            Err(_) => bail!(
                "CDP method {method} timed out after {}s (via daemon)",
                timeout_duration.as_secs()
            ),
        }
    }

    pub async fn send(&mut self, method: &str, params: Value) -> Result<Value> {
        self.do_send(method, params, None).await
    }

    pub async fn send_to_session(
        &mut self,
        method: &str,
        params: Value,
        session_id: &str,
    ) -> Result<Value> {
        self.do_send(method, params, Some(session_id)).await
    }

    /// Read lines from the daemon socket until we get the response for req_id.
    /// Buffer any events we see along the way.
    async fn recv_response(&mut self, req_id: u64) -> Result<Value> {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self
                .reader
                .read_line(&mut line)
                .await
                .context("daemon socket read error")?;
            if n == 0 {
                bail!("Daemon socket closed");
            }

            let value: Value =
                serde_json::from_str(line.trim()).context("invalid JSON from daemon")?;

            let msg_type = value.get("type").and_then(Value::as_str).unwrap_or("");

            match msg_type {
                "cdp_resp" => {
                    if value.get("req_id").and_then(Value::as_u64) == Some(req_id) {
                        if let Some(err) = value.get("error").and_then(Value::as_str) {
                            bail!("{err}");
                        }
                        return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                    }
                    // Response for a different req_id (shouldn't happen in single-threaded CLI)
                }
                "cdp_event" => {
                    // Buffer the event
                    if self.pending_events.len() >= MAX_EVENT_BUFFER {
                        self.pending_events.pop_front();
                    }
                    self.pending_events.push_back(value);
                }
                "cdp_disconnected" => {
                    bail!("WebSocket closed");
                }
                _ => {}
            }
        }
    }

    pub async fn next_event(&mut self, wait: Duration) -> Result<Option<Value>> {
        if let Some(v) = self.pending_events.pop_front() {
            return Ok(Some(v));
        }

        let mut line = String::new();
        match tokio::time::timeout(wait, self.reader.read_line(&mut line)).await {
            Ok(Ok(0)) => bail!("Daemon socket closed"),
            Ok(Err(e)) => Err(e).context("daemon socket read error"),
            Ok(Ok(_)) => {
                let value: Value = serde_json::from_str(line.trim())?;
                let msg_type = value.get("type").and_then(Value::as_str).unwrap_or("");
                match msg_type {
                    "cdp_event" => Ok(Some(value)),
                    "cdp_disconnected" => bail!("WebSocket closed"),
                    _ => Ok(None),
                }
            }
            Err(_) => Ok(None), // timeout
        }
    }

    pub async fn close(self) {
        // Just drop — the daemon keeps the WS connection alive for reuse
        drop(self);
    }
}

// ---------------------------------------------------------------------------
// Daemon IPC handler for CDP proxy
// ---------------------------------------------------------------------------

/// Handle a CDP proxy connection from a CLI client.
/// Called from daemon's handle_connection when type="cdp_connect".
pub async fn handle_cdp_connection(
    ws_url: String,
    mut reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    mut writer: tokio::net::unix::OwnedWriteHalf,
    pool: Arc<Mutex<CdpPool>>,
) {
    // Subscribe to events for this ws_url
    let mut event_rx = {
        let mut p = pool.lock().await;
        p.acquire_client(&ws_url);
        p.subscribe(&ws_url)
    };

    let mut line = String::new();
    let mut auto_dialog: Option<(bool, String)> = None;

    loop {
        tokio::select! {
            // CLI sends a cdp_send request
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) | Err(_) => break, // CLI disconnected
                    Ok(_) => {
                        if let Ok(cmd) = serde_json::from_str::<Value>(line.trim()) {
                            let cmd_type = cmd.get("type").and_then(Value::as_str).unwrap_or("");
                            if cmd_type == "cdp_send" {
                                let method = cmd.get("method").and_then(Value::as_str).unwrap_or("");
                                let params = cmd.get("params").cloned().unwrap_or(json!({}));
                                let session_id = cmd.get("session_id").and_then(Value::as_str);
                                let req_id = cmd.get("req_id").and_then(Value::as_u64).unwrap_or(0);
                                auto_dialog = cmd
                                    .get("auto_dialog")
                                    .and_then(Value::as_object)
                                    .and_then(|obj| {
                                        Some((
                                            obj.get("accept")?.as_bool()?,
                                            obj.get("prompt_text")
                                                .and_then(Value::as_str)
                                                .unwrap_or_default()
                                                .to_string(),
                                        ))
                                    });

                                // Dispatch under lock (brief), then await response without lock
                                let rx = {
                                    let mut p = pool.lock().await;
                                    p.dispatch_cdp(&ws_url, method, params, session_id)
                                };
                                let mut response_rx = match rx {
                                    Ok(rx) => rx,
                                    Err(e) => {
                                        let response = json!({
                                            "type": "cdp_resp",
                                            "req_id": req_id,
                                            "error": format!("{e:#}"),
                                        });
                                        if write_daemon_line(&mut writer, &response).await.is_err() {
                                            break;
                                        }
                                        line.clear();
                                        continue;
                                    }
                                };

                                loop {
                                    tokio::select! {
                                        result = tokio::time::timeout(Duration::from_secs(120), &mut response_rx) => {
                                            let result = match result {
                                                Ok(Ok(val)) => val,
                                                Ok(Err(_)) => Err(anyhow::anyhow!("CDP response dropped")),
                                                Err(_) => Err(anyhow::anyhow!("CDP response timed out after 120s")),
                                            };

                                            let response = match result {
                                                Ok(val) => json!({
                                                    "type": "cdp_resp",
                                                    "req_id": req_id,
                                                    "result": val,
                                                }),
                                                Err(e) => json!({
                                                    "type": "cdp_resp",
                                                    "req_id": req_id,
                                                    "error": format!("{e:#}"),
                                                }),
                                            };

                                            if write_daemon_line(&mut writer, &response).await.is_err() {
                                                pool.lock().await.release_client(&ws_url);
                                                return;
                                            }
                                            break;
                                        }
                                        event = event_rx.recv() => {
                                            let Some(value) = event else {
                                                pool.lock().await.release_client(&ws_url);
                                                return;
                                            };
                                            match process_pool_event(
                                                &pool,
                                                &ws_url,
                                                &mut writer,
                                                &auto_dialog,
                                                value,
                                            ).await {
                                                Ok(true) => {}
                                                Ok(false) | Err(_) => {
                                                    pool.lock().await.release_client(&ws_url);
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        line.clear();
                    }
                }
            }

            // Forward events from daemon's connection pool to CLI
            event = event_rx.recv() => {
                match event {
                    Some(value) => {
                        match process_pool_event(
                            &pool,
                            &ws_url,
                            &mut writer,
                            &auto_dialog,
                            value,
                        ).await {
                            Ok(true) => {}
                            Ok(false) | Err(_) => {
                                break;
                            }
                        }
                    }
                    None => break, // Connection pool dropped the sender
                }
            }
        }
    }

    pool.lock().await.release_client(&ws_url);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn write_daemon_line(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    value: &Value,
) -> std::io::Result<()> {
    let mut out = serde_json::to_string(value).unwrap_or_default();
    out.push('\n');
    writer.write_all(out.as_bytes()).await?;
    writer.flush().await
}

async fn process_pool_event(
    pool: &Arc<Mutex<CdpPool>>,
    ws_url: &str,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    auto_dialog: &Option<(bool, String)>,
    value: Value,
) -> Result<bool> {
    if value.get("type").and_then(Value::as_str) == Some("cdp_disconnected") {
        write_daemon_line(writer, &json!({"type": "cdp_disconnected"})).await?;
        return Ok(false);
    }

    if value.get("method").and_then(Value::as_str) == Some("Page.javascriptDialogOpening")
        && let Some((accept, prompt_text)) = auto_dialog
    {
        let mut params = json!({ "accept": *accept });
        if !prompt_text.is_empty() {
            params["promptText"] = json!(prompt_text);
        }
        let rx = {
            let mut p = pool.lock().await;
            p.dispatch_cdp(ws_url, "Page.handleJavaScriptDialog", params, None)?
        };
        let _ = tokio::time::timeout(Duration::from_secs(10), rx).await;
        return Ok(true);
    }

    let wrapper = json!({
        "type": "cdp_event",
        "method": value.get("method").cloned().unwrap_or(Value::Null),
        "params": value.get("params").cloned().unwrap_or(Value::Null),
    });
    write_daemon_line(writer, &wrapper).await?;
    Ok(true)
}
