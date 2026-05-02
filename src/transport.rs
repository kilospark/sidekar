//! Transport trait and implementations for message delivery.
//!
//! A transport is a way to deliver a message string to an agent.
//! The message model ([`crate::message`]) is transport-independent;
//! transports only care about getting bytes to a destination.

use crate::message::{DeliveryResult, Envelope};
use anyhow::{Context, Result};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// A mechanism for delivering messages to an agent.
pub trait Transport: Send + Sync {
    /// Deliver a pre-formatted message to `target`.
    ///
    /// What `target` means depends on the transport:
    /// - [`Broker`]: recipient agent name
    fn deliver(&self, target: &str, message: &str, from: &str) -> Result<DeliveryResult>;

    /// Transport name for logging.
    fn name(&self) -> &'static str;
}

// ---------------------------------------------------------------------------
// Broker transport (SQLite queue)
// ---------------------------------------------------------------------------

/// Delivers messages by inserting into the SQLite bus_queue table.
/// The recipient's poller picks up and delivers the message.
pub struct Broker;

impl Transport for Broker {
    fn deliver(&self, target: &str, message: &str, from: &str) -> Result<DeliveryResult> {
        match crate::broker::enqueue_message(from, target, message) {
            Ok(()) => Ok(DeliveryResult::Delivered),
            Err(e) => Ok(DeliveryResult::Failed(e.to_string())),
        }
    }

    fn name(&self) -> &'static str {
        "broker"
    }
}

// ---------------------------------------------------------------------------
// Relay HTTP (device token → relay fans out to multiplex tunnels)
// ---------------------------------------------------------------------------

/// HTTPS API base derived from `SIDEKAR_RELAY_URL` (e.g. `https://relay.sidekar.dev`).
pub(crate) fn relay_http_base() -> String {
    let u = std::env::var("SIDEKAR_RELAY_URL")
        .unwrap_or_else(|_| "wss://relay.sidekar.dev/tunnel".into());
    u.replace("wss://", "https://")
        .replace("ws://", "http://")
        .trim_end_matches("/tunnel")
        .trim_end_matches('/')
        .to_string()
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct RelaySessionInfo {
    pub id: String,
    pub name: String,
    pub nickname: Option<String>,
    pub hostname: String,
}

pub(crate) fn fetch_relay_sessions() -> Result<Vec<RelaySessionInfo>> {
    let token = crate::auth::auth_token().ok_or_else(|| anyhow::anyhow!("no device token"))?;
    let base = relay_http_base();
    let url = format!("{}/sessions", base.trim_end_matches('/'));

    // Run blocking HTTP on a dedicated OS thread to avoid panicking when
    // called from within a tokio runtime (reqwest::blocking creates its own runtime).
    std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("relay /sessions: HTTP {}", resp.status());
        }
        #[derive(serde::Deserialize)]
        struct Resp {
            sessions: Vec<RelaySessionInfo>,
        }
        let r: Resp = resp.json().context("parse relay /sessions JSON")?;
        Ok(r.sessions)
    })
    .join()
    .map_err(|_| anyhow::anyhow!("fetch_relay_sessions thread panicked"))?
}

pub struct RelayHttp;

impl Transport for RelayHttp {
    fn deliver(&self, target: &str, message: &str, from: &str) -> Result<DeliveryResult> {
        let token = crate::auth::auth_token()
            .ok_or_else(|| anyhow::anyhow!("no device token; run: sidekar device login"))?;
        let url = format!("{}/relay/bus", relay_http_base().trim_end_matches('/'));
        let target = target.to_string();
        let from = from.to_string();
        let message = message.to_string();

        // Run blocking HTTP on a dedicated OS thread to avoid panicking when
        // called from within a tokio runtime (reqwest::blocking creates its own runtime).
        std::thread::spawn(move || {
            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()?;
            let resp = client
                .post(&url)
                .header("Authorization", format!("Bearer {token}"))
                .json(&serde_json::json!({
                    "recipient_session_id": target,
                    "sender": from,
                    "body": message,
                }))
                .send()
                .with_context(|| format!("POST {url}"))?;
            if resp.status().is_success() {
                Ok(DeliveryResult::Delivered)
            } else {
                let status = resp.status();
                let body = resp.text().unwrap_or_default();
                Ok(DeliveryResult::Failed(format!(
                    "relay HTTP {status}: {body}"
                )))
            }
        })
        .join()
        .map_err(|_| anyhow::anyhow!("RelayHttp::deliver thread panicked"))?
    }

    fn name(&self) -> &'static str {
        "relay_http"
    }
}

pub fn deliver_relay_envelope(
    target: &str,
    envelope: &Envelope,
    paste_body: &str,
) -> Result<DeliveryResult> {
    let token = crate::auth::auth_token()
        .ok_or_else(|| anyhow::anyhow!("no device token; run: sidekar device login"))?;
    let url = format!("{}/relay/bus", relay_http_base().trim_end_matches('/'));
    let target = target.to_string();
    let paste_body = paste_body.to_string();
    let payload = serde_json::json!({
        "recipient_session_id": target,
        "sender": envelope.from.name,
        "body": paste_body,
        "envelope_json": serde_json::to_string(envelope).context("serialize relay envelope")?,
    });

    std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&payload)
            .send()
            .with_context(|| format!("POST {url}"))?;
        if resp.status().is_success() {
            Ok(DeliveryResult::Delivered)
        } else {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            Ok(DeliveryResult::Failed(format!(
                "relay HTTP {status}: {body}"
            )))
        }
    })
    .join()
    .map_err(|_| anyhow::anyhow!("deliver_relay_envelope thread panicked"))?
}
