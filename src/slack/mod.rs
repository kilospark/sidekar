//! Slack over its Web API.
//!
//! Same reasoning as [`crate::google`]: Slack's web client is a long-lived
//! single-page app that browser automation drives badly, while the Web API is
//! stable, documented and scoped. These call it directly with a token from
//! [`auth`].

pub mod api;
pub mod auth;

use anyhow::{Result, bail};
use serde_json::Value;
use std::time::Duration;

/// Where the Web API lives. Every method is `{BASE}/{method}`.
pub const BASE: &str = "https://slack.com/api";

/// How many times a rate-limited call is retried before giving up.
const RATE_LIMIT_RETRIES: usize = 2;

/// The longest we will sleep on one `Retry-After`. Slack can ask for more on a
/// tier-1 method; past this it is better to fail and say so than to hang.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);

/// A Web API client holding one access token.
pub struct Slack {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Slack {
    /// A client for the stored token, refreshing it first if it rotates.
    pub async fn connect(token: &auth::TokenRef) -> Result<Self> {
        let access = auth::access_token_for(token).await?;
        Ok(Self::new(access))
    }

    pub fn new(access_token: String) -> Self {
        Self::with_base(crate::http_client::client(), BASE, access_token)
    }

    /// Point at another host. Tests use this to aim at a local mock.
    pub fn with_base(http: reqwest::Client, base: &str, access_token: String) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: access_token,
        }
    }

    /// A read method, arguments in the query string.
    pub async fn get(&self, method: &str, params: &[(&str, String)]) -> Result<Value> {
        let url = format!("{}/{method}", self.base);
        self.call(method, || self.http.get(&url).query(params))
            .await
    }

    /// A write method, arguments as a JSON body.
    pub async fn post(&self, method: &str, body: &Value) -> Result<Value> {
        let url = format!("{}/{method}", self.base);
        self.call(method, || {
            self.http
                .post(&url)
                .header("Content-Type", "application/json; charset=utf-8")
                .body(body.to_string())
        })
        .await
    }

    async fn call(
        &self,
        method: &str,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<Value> {
        let mut attempt = 0;
        loop {
            let res = build().bearer_auth(&self.token).send().await?;
            let status = res.status();
            // Slack rate limits per method and says how long to wait. Waiting
            // briefly is cheaper than making every caller handle it.
            if status.as_u16() == 429 && attempt < RATE_LIMIT_RETRIES {
                let wait = res
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(Duration::from_secs)
                    .unwrap_or(Duration::from_secs(1))
                    .min(MAX_RETRY_WAIT);
                tokio::time::sleep(wait).await;
                attempt += 1;
                continue;
            }
            let text = res.text().await.unwrap_or_default();
            return check(method, status, &text);
        }
    }
}

/// Read a Web API response, turning `ok: false` into an error that says what
/// to do about it.
///
/// Slack answers almost every failure with `200 OK` and `{"ok": false}`, so
/// the status code alone says nothing; the `error` field is the whole story.
pub(crate) fn check(method: &str, status: reqwest::StatusCode, text: &str) -> Result<Value> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        bail!(
            "{status} from Slack {method}: {}",
            text.chars().take(300).collect::<String>()
        );
    };
    if v.get("ok").and_then(|o| o.as_bool()) == Some(true) {
        return Ok(v);
    }
    let code = v
        .get("error")
        .and_then(|e| e.as_str())
        .unwrap_or("unknown_error");
    bail!("Slack {method} failed: {}", explain_error(code, &v));
}

/// Slack's error codes are terse; most have a one-line fix.
pub(crate) fn explain_error(code: &str, v: &Value) -> String {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("");
    match code {
        "missing_scope" => format!(
            "missing_scope — this needs {} but the token was granted {}. Add the scope on the \
             app's OAuth & Permissions page, reinstall, then run `sidekar slack login` again.",
            if s("needed").is_empty() {
                "another scope"
            } else {
                s("needed")
            },
            if s("provided").is_empty() {
                "(unknown)"
            } else {
                s("provided")
            },
        ),
        "not_authed" | "invalid_auth" | "token_revoked" | "token_expired" | "account_inactive" => {
            format!(
                "{code} — the stored token no longer works. Run `sidekar slack login` again, or \
                 `sidekar slack add` with a fresh token."
            )
        }
        "not_in_channel" => "not_in_channel — a bot token can only read and post where the bot \
             is a member. Invite it with /invite @<app> in that channel, or use a user token."
            .into(),
        "channel_not_found" => "channel_not_found — no such channel, or the token cannot see it \
             (private channels need groups:read / groups:history and membership)."
            .into(),
        "not_allowed_token_type" => "not_allowed_token_type — this method needs the other kind \
             of token. Search, for one, only works with a user token (`slack login` without --bot)."
            .into(),
        "attached_draft_exists" => "attached_draft_exists — that conversation's composer already \
             holds a draft. Send or discard it in Slack first; a draft cannot be replaced from here."
            .into(),
        "unknown_method" => "unknown_method — Slack no longer offers this method (drafts.create is \
             undocumented and can disappear without notice)."
            .into(),
        "ratelimited" => "ratelimited — Slack is throttling this method; wait a minute.".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests;
