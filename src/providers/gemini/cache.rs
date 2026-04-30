//! Thin HTTP client for Gemini's cachedContents REST API.
//!
//! Reference: https://ai.google.dev/api/caching
//!
//! Three verbs we use:
//!   POST /v1beta/cachedContents               — create a cache.
//!   GET  /v1beta/{name=cachedContents/*}      — look up metadata (rare;
//!                                               normally we trust our
//!                                               local registry).
//!   DELETE /v1beta/{name=cachedContents/*}    — explicit eviction
//!                                               (not called in the
//!                                               current implementation;
//!                                               we let TTLs expire).
//!
//! The caller is responsible for fingerprinting / registry lookup / TTL
//! tracking; this module just marshals JSON.
//!
//! Important wire-format note: the server requires at least ONE of
//! `contents`, `tools`, or `systemInstruction` to be present AND to
//! exceed the model's minimum cacheable token count (4096 for 2.5
//! Flash, 1024 for 2.5 Pro). The create call returns 400 if under.
//! The adapter enforces min-tokens with an estimate before we get
//! here; this module reports any 400 as a soft failure so callers
//! fall through to uncached mode without aborting the turn.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// Response metadata from a successful create.
#[derive(Debug, Clone)]
pub struct CreatedCache {
    /// Fully-qualified name, e.g. `"cachedContents/abc123"`. Pass this
    /// back in subsequent `generateContent` requests as the
    /// `cachedContent` field.
    pub name: String,
    /// Total tokens in the cached payload (from server's usageMetadata).
    pub token_count: u64,
    /// Unix-seconds timestamp at which the server will evict this
    /// cache. The registry stores this so we don't attempt reuse of
    /// an already-expired entry.
    pub expires_at_unix: i64,
}

/// Create a Gemini cached content object. Body shape:
/// ```json
/// {
///   "model": "models/gemini-2.5-pro",
///   "contents": [...],
///   "tools": [{"functionDeclarations": [...]}],
///   "systemInstruction": {"parts": [{"text": "..."}]},
///   "ttl": "3600s",
///   "displayName": "sidekar-session-<fingerprint-prefix>"
/// }
/// ```
pub struct CreateCacheRequest<'a> {
    pub api_key: &'a str,
    pub base_url: &'a str,
    pub model: &'a str,
    pub contents: &'a [Value],
    pub tools: &'a [Value],
    pub system_instruction: Option<&'a Value>,
    pub ttl_secs: u32,
    pub display_name: &'a str,
}

/// Returns `Ok(None)` for 4xx (unretryable, e.g. under min tokens) so
/// callers can fall back to the uncached path. Returns `Err` for 5xx
/// and network errors — caller typically still falls back but logs.
pub async fn create_cache(req: CreateCacheRequest<'_>) -> Result<Option<CreatedCache>> {
    let CreateCacheRequest {
        api_key,
        base_url,
        model,
        contents,
        tools,
        system_instruction,
        ttl_secs,
        display_name,
    } = req;
    let url = format!("{}/cachedContents", base_url.trim_end_matches('/'));

    // Gemini's model field on cachedContents must be the full
    // "models/..." path, not just the short model id. (Asymmetric with
    // the streamGenerateContent URL, which takes the short id.)
    let full_model = if model.starts_with("models/") {
        model.to_string()
    } else {
        format!("models/{model}")
    };

    let mut body = json!({
        "model": full_model,
        "contents": contents,
        "ttl": format!("{ttl_secs}s"),
        "displayName": display_name,
    });
    if !tools.is_empty() {
        body["tools"] = json!([{ "functionDeclarations": tools }]);
    }
    if let Some(sys) = system_instruction {
        body["systemInstruction"] = sys.clone();
    }

    let client = super::super::catalog_http_client(30)
        .map_err(anyhow::Error::msg)
        .context("build http client")?;

    let resp = client
        .post(&url)
        .header("x-goog-api-key", api_key)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("cachedContents create: network")?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        // 400s are input problems (too few tokens, unsupported model,
        // malformed payload). Surface as Ok(None) so the caller
        // gracefully falls back to uncached generateContent.
        if status.is_client_error() {
            eprintln!(
                "gemini cache: create rejected ({status}), falling back to uncached: {}",
                truncate_for_log(&text, 200)
            );
            return Ok(None);
        }
        bail!(
            "cachedContents create failed ({status}): {}",
            truncate_for_log(&text, 500)
        );
    }

    let data: Value = resp
        .json()
        .await
        .context("cachedContents create: decode response")?;

    let name = data
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("cachedContents create response missing `name`"))?
        .to_string();
    let token_count = data
        .get("usageMetadata")
        .and_then(|u| u.get("totalTokenCount"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    // expireTime is RFC 3339 (e.g. "2025-04-21T18:30:00Z"). Rather
    // than add a chrono dep just for this one field, trust our own
    // TTL — Gemini sets expireTime == createTime + ttl. If the server
    // truncates our requested TTL (it caps at 6h), we'll find out by
    // getting a 404 on reuse; the registry handles that by evicting.
    let expires_at_unix = now_unix() + ttl_secs as i64;
    let _ = data.get("expireTime"); // acknowledged but unused

    Ok(Some(CreatedCache {
        name,
        token_count,
        expires_at_unix,
    }))
}

/// Delete a cached content. Used only for explicit eviction; the
/// registry's lazy-expiry path prefers to let the server age them
/// out. Returns Ok even if the object is already gone (404 is
/// treated as success since the caller's intent is "be rid of it").
#[allow(dead_code)]
pub async fn delete_cache(api_key: &str, base_url: &str, name: &str) -> Result<()> {
    let url = format!("{}/{name}", base_url.trim_end_matches('/'));
    let client = super::super::catalog_http_client(10).map_err(anyhow::Error::msg)?;
    let resp = client
        .delete(&url)
        .header("x-goog-api-key", api_key)
        .send()
        .await?;
    if resp.status().is_success() || resp.status().as_u16() == 404 {
        Ok(())
    } else {
        bail!(
            "cachedContents delete failed ({}): {}",
            resp.status(),
            truncate_for_log(&resp.text().await.unwrap_or_default(), 200)
        )
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn truncate_for_log(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}... [{} bytes truncated]", &s[..max], s.len() - max)
    }
}
