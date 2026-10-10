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
    /// Where resolved names are remembered, per workspace. `None` (tests,
    /// unknown workspace) means no caching.
    cache: Option<std::path::PathBuf>,
}

/// How long a resolved name → id stays trusted. Long enough that a burst of
/// commands looks a channel up once; short enough that a rename or a new
/// person shows up within minutes.
pub(crate) const NAME_CACHE_TTL_SECS: u64 = 15 * 60;

impl Slack {
    /// A client for the stored token, refreshing it first if it rotates.
    pub async fn connect(token: &auth::TokenRef) -> Result<Self> {
        let access = auth::access_token_for(token).await?;
        let scope = if token.team_id.is_empty() {
            &token.key
        } else {
            &token.team_id
        };
        Ok(Self::new(access).with_cache(Some(name_cache_path(scope))))
    }

    /// Use (or stop using) a name cache file.
    pub fn with_cache(mut self, path: Option<std::path::PathBuf>) -> Self {
        self.cache = path;
        self
    }

    /// A remembered id for `key`, if fresh.
    pub(crate) fn cache_get(&self, key: &str) -> Option<String> {
        let path = self.cache.as_ref()?;
        let map: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        let e = map.get(key)?;
        let at = e.get("at")?.as_u64()?;
        if crate::oauth_loopback::now_secs().saturating_sub(at) > NAME_CACHE_TTL_SECS {
            return None;
        }
        e.get("id")?.as_str().map(String::from)
    }

    /// Remember `key` → `id`. Best effort: a cache that cannot be written
    /// only costs a lookup next time.
    pub(crate) fn cache_put(&self, key: &str, id: &str) {
        let Some(path) = self.cache.as_ref() else {
            return;
        };
        let now = crate::oauth_loopback::now_secs();
        let mut map: serde_json::Map<String, Value> = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        map.retain(|_, e| {
            e.get("at")
                .and_then(|a| a.as_u64())
                .is_some_and(|at| now.saturating_sub(at) <= NAME_CACHE_TTL_SECS)
        });
        map.insert(key.to_string(), serde_json::json!({"id": id, "at": now}));
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, Value::Object(map).to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
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
            cache: None,
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

    /// A method that only takes form fields (`files.getUploadURLExternal`
    /// and `files.completeUploadExternal` do not read JSON bodies).
    pub async fn post_form(&self, method: &str, fields: &[(&str, String)]) -> Result<Value> {
        let url = format!("{}/{method}", self.base);
        self.call(method, || self.http.post(&url).form(fields))
            .await
    }

    /// Fetch a private file (`url_private_download`) with the token.
    ///
    /// The token goes only to Slack's own hosts over https (or the test
    /// mock's host). Without `files:read` Slack answers a file URL with its
    /// sign-in page rather than an error, so an HTML reply to a non-HTML file
    /// is reported as the scope problem it is.
    pub async fn download(&self, url: &str, expect_html: bool) -> Result<Vec<u8>> {
        let test_host = crate::attachments::host_of(&self.base);
        let test_host = test_host.filter(|h| h != "slack.com");
        if !crate::attachments::token_may_go_to(url, "slack.com", test_host.as_deref()) {
            bail!("refusing to send the Slack token to {url}: not a Slack file URL");
        }
        let res = self.http.get(url).bearer_auth(&self.token).send().await?;
        let status = res.status();
        let html = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.starts_with("text/html"));
        if !status.is_success() {
            bail!("{status} downloading {url}");
        }
        if html && !expect_html {
            bail!(
                "Slack sent a web page instead of the file, which is what it does when the token \
                 lacks files:read. Add files:read to the app (`sidekar slack setup` has the \
                 manifest), reinstall, and run `sidekar slack login` again."
            );
        }
        Ok(res.bytes().await?.to_vec())
    }

    /// Send bytes to a pre-signed upload URL. The URL carries its own
    /// authorization, so the token is not sent.
    pub async fn put_upload(&self, upload_url: &str, bytes: Vec<u8>) -> Result<()> {
        let res = self
            .http
            .post(upload_url)
            .header("Content-Type", "application/octet-stream")
            .body(bytes)
            .send()
            .await?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            bail!(
                "{status} uploading to Slack: {}",
                text.chars().take(300).collect::<String>()
            );
        }
        Ok(())
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

/// The name cache for one workspace: `~/.sidekar/cache/slack-names-<team>.json`.
pub(crate) fn name_cache_path(scope: &str) -> std::path::PathBuf {
    let safe: String = scope
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".sidekar")
        .join("cache")
        .join(format!("slack-names-{safe}.json"))
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
