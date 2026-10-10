//! Linear sign-in, keyed by kv entry so several workspaces can be connected.
//!
//! Mirrors [`crate::google::auth`] and [`crate::slack::auth`]:
//!
//! ```text
//! <TOKEN_KEY>            a personal API key, or a JSON blob holding an OAuth
//!                        access token, its rotating refresh token and expiry
//!   tags: linear, linear-token, auth:apikey|oauth, acct:<email>, org:<name>,
//!         client-id:<KEY>, client-secret:<KEY>   (only when `login` minted it)
//! LINEAR_DEFAULT_TOKEN   which token key commands use by default
//! ```
//!
//! Two ways in:
//!
//! - `linear add` adopts a personal API key (Settings → Security & access →
//!   Personal API keys) already in kv. Simplest; acts as you; never expires.
//! - `linear login` runs OAuth against an app the user created. Linear
//!   compares the redirect against the app's registered URIs exactly, so the
//!   listener uses a fixed port (default [`DEFAULT_PORT`]). Since April 2026
//!   every Linear OAuth token lasts 24 hours and comes with a refresh token
//!   that rotates on use; the blob keeps all three and refreshes on demand.

use super::Linear;
use crate::oauth_loopback::{CONSENT_TIMEOUT, ExpiringToken};
use anyhow::{Result, bail};
use serde_json::Value;

const DEFAULT_TOKEN_KEY: &str = "LINEAR_DEFAULT_TOKEN";

/// The loopback port `linear login` listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 53695;

pub const AUTHORIZE_URL: &str = "https://linear.app/oauth/authorize";
pub const TOKEN_URL: &str = "https://api.linear.app/oauth/token";

/// `write` covers creating and updating issues and comments; Linear has no
/// delete scope separate from `admin`, which is deliberately not asked for.
pub const SCOPES: &str = "read,write";

const MARKER_TAG: &str = "linear-token";
const METHOD_TAG: &str = "auth:";
const ACCOUNT_TAG: &str = "acct:";
const ORG_TAG: &str = "org:";
const CLIENT_ID_TAG: &str = "client-id:";
const CLIENT_SECRET_TAG: &str = "client-secret:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    ApiKey,
    OAuth,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::ApiKey => "apikey",
            Method::OAuth => "oauth",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRef {
    pub key: String,
    pub method: Method,
    pub account: String,
    pub org: String,
    pub client_id_key: Option<String>,
    pub client_secret_key: Option<String>,
}

pub(crate) fn tags_for(
    method: Method,
    account: &str,
    org: &str,
    client_keys: Option<(&str, &str)>,
) -> Vec<String> {
    let mut tags = vec![
        "linear".to_string(),
        MARKER_TAG.to_string(),
        format!("{METHOD_TAG}{}", method.as_str()),
        format!("{ACCOUNT_TAG}{account}"),
        format!("{ORG_TAG}{org}"),
    ];
    if let Some((id, secret)) = client_keys {
        tags.push(format!("{CLIENT_ID_TAG}{id}"));
        tags.push(format!("{CLIENT_SECRET_TAG}{secret}"));
    }
    tags
}

pub(crate) fn token_ref_from(key: &str, tags: &[String]) -> Option<TokenRef> {
    if !tags.iter().any(|t| t == MARKER_TAG) {
        return None;
    }
    let find = |p: &str| {
        tags.iter()
            .find_map(|t| t.strip_prefix(p))
            .map(String::from)
    };
    Some(TokenRef {
        key: key.to_string(),
        method: match find(METHOD_TAG).as_deref() {
            Some("oauth") => Method::OAuth,
            _ => Method::ApiKey,
        },
        account: find(ACCOUNT_TAG).unwrap_or_default(),
        org: find(ORG_TAG).unwrap_or_default(),
        client_id_key: find(CLIENT_ID_TAG),
        client_secret_key: find(CLIENT_SECRET_TAG),
    })
}

fn kv(key: &str) -> Result<Option<String>> {
    Ok(crate::broker::kv_get(key)?.map(|e| e.value))
}

pub fn tokens() -> Result<Vec<TokenRef>> {
    let listing = crate::broker::kv_scan(None)?;
    let mut out: Vec<TokenRef> = listing
        .keys()
        .into_iter()
        .filter_map(|(key, tags)| token_ref_from(key, tags))
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

pub fn default_token_key() -> Result<Option<String>> {
    kv(DEFAULT_TOKEN_KEY)
}

pub fn set_default_token_key(key: &str) -> Result<()> {
    crate::broker::kv_set(DEFAULT_TOKEN_KEY, key, Some(&["linear".into()]))
}

/// `--token`, then the default, then a sole token; several and no default is
/// an error rather than a guess.
pub fn resolve_token(requested: Option<&str>) -> Result<TokenRef> {
    pick_token(tokens()?, default_token_key()?, requested)
}

pub(crate) fn pick_token(
    stored: Vec<TokenRef>,
    default: Option<String>,
    requested: Option<&str>,
) -> Result<TokenRef> {
    if let Some(k) = requested {
        return stored.into_iter().find(|t| t.key == k).ok_or_else(|| {
            anyhow::anyhow!(
                "no Linear token stored under {k}; run `sidekar linear add --token {k}` or \
                 `sidekar linear login --token {k} …`"
            )
        });
    }
    if let Some(d) = default
        && let Some(found) = stored.iter().find(|t| t.key == d)
    {
        return Ok(found.clone());
    }
    match stored.len() {
        0 => bail!(
            "no Linear token stored. Run `sidekar linear setup` for the steps, then \
             `sidekar linear add` (API key) or `sidekar linear login` (OAuth)."
        ),
        1 => Ok(stored.into_iter().next().unwrap()),
        _ => bail!(
            "several Linear tokens are stored ({}). Pass --token <KV_KEY>, or pick a default \
             with `sidekar linear use <KV_KEY>`.",
            stored
                .iter()
                .map(|t| t.key.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub fn forget(token_key: &str) -> Result<()> {
    let _ = crate::broker::kv_delete(token_key);
    if default_token_key()?.as_deref() == Some(token_key) {
        let _ = crate::broker::kv_delete(DEFAULT_TOKEN_KEY);
    }
    Ok(())
}

pub fn redirect_uri(port: u16) -> String {
    format!("http://localhost:{port}/callback")
}

pub(crate) fn authorize_url(client_id: &str, redirect: &str, state: &str) -> String {
    format!(
        "{AUTHORIZE_URL}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}\
         &prompt=consent",
        urlencoding::encode(client_id),
        urlencoding::encode(redirect),
        urlencoding::encode(SCOPES),
        urlencoding::encode(state),
    )
}

/// The `Authorization` header for a raw credential. Personal API keys go
/// bare, as Linear documents; anything else is an OAuth token.
pub(crate) fn header_for(credential: &str) -> String {
    let c = credential.trim();
    if c.starts_with("lin_api_") {
        c.to_string()
    } else {
        format!("Bearer {c}")
    }
}

pub struct LoginOptions<'a> {
    pub token_key: &'a str,
    pub client_id_key: &'a str,
    pub client_secret_key: &'a str,
    pub port: u16,
    pub open_browser: bool,
}

/// Run the OAuth consent flow and store the token under `token_key`.
pub async fn login(opts: LoginOptions<'_>) -> Result<super::api::Viewer> {
    let client_id = kv(opts.client_id_key)?
        .ok_or_else(|| anyhow::anyhow!("{} is not in sidekar kv", opts.client_id_key))?;
    let client_secret = kv(opts.client_secret_key)?
        .ok_or_else(|| anyhow::anyhow!("{} is not in sidekar kv", opts.client_secret_key))?;

    let listeners = crate::oauth_loopback::bind_localhost(opts.port)?;
    let redirect = redirect_uri(opts.port);
    let state = crate::message::gen_msg_id();
    let url = authorize_url(&client_id, &redirect, &state);

    println!("Open this URL to authorize:\n  {url}\n");
    println!(
        "Listening on {redirect} for up to {}s. That exact URL must be one of the app's \
         Callback URLs.",
        CONSENT_TIMEOUT.as_secs()
    );
    if opts.open_browser && !crate::oauth_loopback::open_in_browser(&url) {
        println!("(could not launch a browser here; use the URL above)");
    }

    let code = crate::oauth_loopback::wait_for_code(listeners, &state, "Linear", CONSENT_TIMEOUT)?;
    let now = crate::oauth_loopback::now_secs();
    let res = token_request(
        &crate::http_client::client(),
        TOKEN_URL,
        &[
            ("code", code.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("grant_type", "authorization_code"),
        ],
    )
    .await?;
    let token = ExpiringToken::from_response(&res, now)
        .ok_or_else(|| anyhow::anyhow!("Linear returned no access token"))?;

    let viewer = super::api::viewer(&Linear::new(header_for(&token.access_token))).await?;
    crate::broker::kv_set(
        opts.token_key,
        &token.to_value(),
        Some(&tags_for(
            Method::OAuth,
            &viewer.email,
            &viewer.org,
            Some((opts.client_id_key, opts.client_secret_key)),
        )),
    )?;
    if default_token_key()?.is_none() {
        set_default_token_key(opts.token_key)?;
    }
    Ok(viewer)
}

/// Adopt a credential the user already put in kv under `token_key`: a personal
/// API key, or an OAuth access token they minted elsewhere.
pub async fn add(token_key: &str) -> Result<(Method, super::api::Viewer)> {
    let entry = crate::broker::kv_get(token_key)?.ok_or_else(|| {
        anyhow::anyhow!(
            "{token_key} is not in sidekar kv. Store the key first:\n  \
             sidekar kv set {token_key} 'lin_api_…'"
        )
    })?;
    let credential = ExpiringToken::parse(&entry.value).access_token;
    let method = if credential.starts_with("lin_api_") {
        Method::ApiKey
    } else {
        Method::OAuth
    };
    let viewer = super::api::viewer(&Linear::new(header_for(&credential))).await?;
    let mut tags = entry.tags.clone();
    for t in tags_for(method, &viewer.email, &viewer.org, None) {
        if !tags.contains(&t) {
            tags.push(t);
        }
    }
    crate::broker::kv_set(token_key, &entry.value, Some(&tags))?;
    if default_token_key()?.is_none() {
        set_default_token_key(token_key)?;
    }
    Ok((method, viewer))
}

/// POST to Linear's token endpoint, form-encoded.
pub(crate) async fn token_request(
    http: &reqwest::Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<Value> {
    let res = http.post(url).form(form).send().await?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    let v: Value = serde_json::from_str(&text).map_err(|_| {
        anyhow::anyhow!(
            "{status} from Linear's token endpoint: {}",
            text.chars().take(300).collect::<String>()
        )
    })?;
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        let detail = v
            .get("error_description")
            .and_then(|d| d.as_str())
            .unwrap_or("");
        bail!("Linear token request failed: {err} {detail}");
    }
    if !status.is_success() {
        bail!("{status} from Linear's token endpoint");
    }
    Ok(v)
}

/// The `Authorization` header to call with, refreshing an OAuth token first
/// when it is near expiry.
pub async fn authorization_for(token: &TokenRef) -> Result<String> {
    let stored = kv(&token.key)?
        .ok_or_else(|| anyhow::anyhow!("no Linear token stored under {}", token.key))?;
    let current = ExpiringToken::parse(&stored);
    let now = crate::oauth_loopback::now_secs();
    if !current.needs_refresh(now) {
        return Ok(header_for(&current.access_token));
    }
    let (Some(id_key), Some(secret_key)) = (&token.client_id_key, &token.client_secret_key) else {
        bail!(
            "the Linear token in {} has expired and was not minted by `linear login`, so there is \
             no client to refresh it with.",
            token.key
        );
    };
    let client_id = kv(id_key)?.ok_or_else(|| anyhow::anyhow!("{id_key} is not in sidekar kv"))?;
    let client_secret =
        kv(secret_key)?.ok_or_else(|| anyhow::anyhow!("{secret_key} is not in sidekar kv"))?;
    let refresh = current.refresh_token.clone().unwrap_or_default();
    let res = token_request(
        &crate::http_client::client(),
        TOKEN_URL,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
        ],
    )
    .await
    .map_err(|e| {
        anyhow::anyhow!(
            "could not refresh the Linear token in {}: {e}\nRe-authorize with `sidekar linear \
             login --token {} --client-id {id_key} --client-secret {secret_key}`.",
            token.key,
            token.key
        )
    })?;
    let next = refreshed(&res, &current, now)?;
    // Linear rotates the refresh token on every use, so the new one has to be
    // written back before anything else can fail: losing it means logging in
    // again.
    crate::broker::kv_set(&token.key, &next.to_value(), None)?;
    Ok(header_for(&next.access_token))
}

pub(crate) fn refreshed(v: &Value, previous: &ExpiringToken, now: u64) -> Result<ExpiringToken> {
    let mut next = ExpiringToken::from_response(v, now)
        .ok_or_else(|| anyhow::anyhow!("Linear's refresh returned no access token"))?;
    if next.refresh_token.is_none() {
        next.refresh_token = previous.refresh_token.clone();
    }
    Ok(next)
}

#[cfg(test)]
mod tests;
