//! Cursor backend wire — **Rust only**, aimed at MitM-visible `CURSOR_BACKEND` (default
//! **`https://api2.cursor.sh`**) Connect-style paths from `@cursor/sdk` bundles + Sidekar `proxy_log`.
//!
//! **What MitM / SDK show**
//! - JSON `POST /auth/exchange_user_api_key` → `accessToken` (Bearer for RPCs).
//! - Protobuf **`agent.v1.AgentService`**: **`Run`** (BiDiStreaming), **`RunSSE`**, **`RunPoll`**.
//! - `RunSSE` / `RunPoll` use **`aiserver.v1.BidiRequestId`** (`request_id` string) **after** `Run` bootstraps
//!   the session — you cannot fabricate the id without walking `Run`.
//! - A REPL turn means encoding **`agent.v1.AgentClientMessage`** (large oneof + tools). Schema is not public;
//!   you recover it from **raw `proxy_log` bodies** or **JS protobuf field tables**, then `prost` codegen.
//!
//! **This file today**
//! - Implements **exchange** (+ optional **`SIDEKAR_CURSOR_CONNECT_PROBE=1`** unary `GetUsableModels` over
//!   `application/connect+proto`).
//! - **`stream`**: returns an explicit error describing the missing `Run` protobuf work — no cloud API, no Node.
//!
//! Env: `CURSOR_BACKEND_URL` / `CURSOR_API_BASE_URL`, `SIDEKAR_CURSOR_CONNECT_PROBE`.

use anyhow::{Context, Result, bail};
use std::time::Duration;
use tokio::sync::mpsc;

use super::{
    AssistantResponse, ChatMessage, ContentBlock, StopReason, StreamEvent, ToolDef, Usage,
    build_streaming_client,
};

fn backend_base() -> String {
    std::env::var("CURSOR_BACKEND_URL")
        .or_else(|_| std::env::var("CURSOR_API_BASE_URL"))
        .unwrap_or_else(|_| "https://api2.cursor.sh".into())
        .trim_end_matches('/')
        .to_string()
}

/// Connect unary envelope: 1 byte flags + 4 BE length + payload (protobuf).
fn connect_unary_envelope(payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(5 + payload.len());
    v.push(0);
    v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    v.extend_from_slice(payload);
    v
}

async fn exchange_access_token(
    client: &reqwest::Client,
    api_key: &str,
    base: &str,
) -> Result<String> {
    let url = format!("{base}/auth/exchange_user_api_key");
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .context("cursor exchange_user_api_key connect")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let t = resp.text().await.unwrap_or_default();
        bail!("exchange_user_api_key {}: {}", status, t);
    }
    let v: serde_json::Value = resp.json().await.context("exchange json")?;
    v.get("accessToken")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .context("exchange response missing accessToken")
}

fn cursor_client_headers(bearer: &str) -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderValue};
    let mut h = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&format!("Bearer {bearer}")) {
        h.insert(reqwest::header::AUTHORIZATION, v);
    }
    h.insert("x-cursor-client-type", HeaderValue::from_static("sdk"));
    h.insert(
        "x-cursor-client-version",
        HeaderValue::from_static(concat!("sidekar-", env!("CARGO_PKG_VERSION"))),
    );
    h.insert("x-ghost-mode", HeaderValue::from_static("true"));
    h
}

/// Empty protobuf message — valid when RPC has zero fields.
async fn probe_get_usable_models(client: &reqwest::Client, bearer: &str, base: &str) -> Result<()> {
    let url = format!("{base}/agent.v1.AgentService/GetUsableModels");
    let body = connect_unary_envelope(&[]);
    let resp = client
        .post(url)
        .headers(cursor_client_headers(bearer))
        .header("Content-Type", "application/connect+proto")
        .header("Accept", "application/connect+proto")
        .header("Connect-Protocol-Version", "1")
        .body(body)
        .send()
        .await
        .context("GetUsableModels transport")?;
    let status = resp.status();
    let bytes = resp.bytes().await.context("GetUsableModels body")?;
    if !status.is_success() {
        bail!(
            "GetUsableModels {}: {}",
            status,
            String::from_utf8_lossy(&bytes[..bytes.len().min(384)])
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn stream(
    api_key: &str,
    _model: &str,
    _system_prompt: &str,
    _messages: &[ChatMessage],
    _tools: &[ToolDef],
    _prompt_cache_key: Option<&str>,
) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
    let (tx, rx) = mpsc::unbounded_channel();
    let _ = tx.send(StreamEvent::Waiting);
    let _ = tx.send(StreamEvent::Connecting);

    let api_key = api_key.to_string();
    let base = backend_base();
    let probe = std::env::var("SIDEKAR_CURSOR_CONNECT_PROBE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    tokio::spawn(async move {
        let err: Result<()> = async {
            let client =
                build_streaming_client(Duration::from_secs(300)).context("cursor http client MITM")?;
            let bearer = exchange_access_token(&client, &api_key, &base).await?;
            if probe {
                probe_get_usable_models(&client, &bearer, &base).await?;
            }
            bail!(
                "Cursor Rust REPL stream blocked: needs `prost` schemas for `agent.v1.AgentService/Run` (BiDi) + `AgentClientMessage` / `AgentServerMessage`. MitM `RunSSE` is not standalone — `request_id` comes from `Run`. Export bodies from `sidekar proxy show <id>` or codegen from Cursor SDK protobuf metadata, then extend this module.\n\
                 Probe only: set SIDEKAR_CURSOR_CONNECT_PROBE=1 to hit `GetUsableModels` after exchange.\n\
                 Backend: {base}"
            );
        }
        .await;

        if let Err(e) = err {
            let _ = tx.send(StreamEvent::Error {
                message: format!("{e:#}"),
            });
            let _ = tx.send(StreamEvent::Done {
                message: AssistantResponse {
                    content: vec![ContentBlock::Text {
                        text: String::new(),
                    }],
                    usage: Usage::default(),
                    stop_reason: StopReason::Stop,
                    model: String::new(),
                    response_id: String::new(),
                    rate_limit: None,
                },
            });
        }
    });

    Ok(rx)
}
