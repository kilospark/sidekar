//! Slack sign-in, keyed by kv entry so several workspaces can be connected.
//!
//! Mirrors [`crate::google::auth`]: the caller names every kv key, a token
//! entry carries tags recording what it is, and one entry can be the default.
//!
//! ```text
//! <TOKEN_KEY>            the access token (or a JSON blob when it rotates)
//!   tags: slack, slack-token, kind:user|bot, team:<name>, team-id:<id>,
//!         acct:<user>, url:<workspace url>,
//!         client-id:<KEY>, client-secret:<KEY>   (only when `login` minted it)
//! SLACK_DEFAULT_TOKEN    which token key commands use by default
//! ```
//!
//! Two ways in:
//!
//! - `slack login` runs OAuth v2 against an app the user created, catching the
//!   redirect on `http://localhost:<port>/callback`. Slack matches the redirect
//!   against URLs registered on the app, port included, so the port is fixed
//!   (default [`DEFAULT_PORT`]) rather than picked by the OS as Google's is.
//! - `slack add` adopts a token already in kv (an `xoxp-` user token or an
//!   `xoxb-` bot token copied from the app's settings page), checks it with
//!   `auth.test`, and tags it.
//!
//! A user token acts as the person, which is what Gmail does and what an agent
//! working on someone's behalf wants: it sees their channels and DMs and can
//! search. A bot token acts as the app and sees only where it was invited.

use super::Slack;
use crate::oauth_loopback::{CONSENT_TIMEOUT, ExpiringToken};
use anyhow::{Result, bail};
use serde_json::Value;

/// Key holding the token key to use when none is named.
const DEFAULT_TOKEN_KEY: &str = "SLACK_DEFAULT_TOKEN";

/// The loopback port `slack login` listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 53694;

/// Marks a kv entry as a Slack token, as opposed to anything else tagged slack.
const MARKER_TAG: &str = "slack-token";
const KIND_TAG: &str = "kind:";
const TEAM_TAG: &str = "team:";
const TEAM_ID_TAG: &str = "team-id:";
const ACCOUNT_TAG: &str = "acct:";
const URL_TAG: &str = "url:";
const CLIENT_ID_TAG: &str = "client-id:";
const CLIENT_SECRET_TAG: &str = "client-secret:";

/// Scopes for a user token: read every conversation the person can, post as
/// them, open DMs, look people up, search, read channel bookmarks, and read
/// and upload files.
///
/// Drafts (`drafts.create`) need no scope of their own beyond a user token;
/// `im:write` lets a draft target a DM that does not exist yet.
pub const USER_SCOPES: &[&str] = &[
    "channels:read",
    "groups:read",
    "im:read",
    "mpim:read",
    "channels:history",
    "groups:history",
    "im:history",
    "mpim:history",
    "chat:write",
    "im:write",
    "users:read",
    "users:read.email",
    "search:read",
    "bookmarks:read",
    "files:read",
    "files:write",
];

/// Scopes for a bot token. The same, less search: Slack does not offer
/// `search:read` to bots at all.
pub const BOT_SCOPES: &[&str] = &[
    "channels:read",
    "groups:read",
    "im:read",
    "mpim:read",
    "channels:history",
    "groups:history",
    "im:history",
    "mpim:history",
    "chat:write",
    "im:write",
    "users:read",
    "users:read.email",
    "bookmarks:read",
    "files:read",
    "files:write",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    User,
    Bot,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::User => "user",
            Kind::Bot => "bot",
        }
    }

    /// Slack prefixes tells which is which without a network call.
    pub fn from_token(token: &str) -> Option<Kind> {
        if token.starts_with("xoxb-") || token.starts_with("xoxe.xoxb-") {
            Some(Kind::Bot)
        } else if token.starts_with("xoxp-") || token.starts_with("xoxe.xoxp-") {
            Some(Kind::User)
        } else {
            None
        }
    }
}

/// What a stored Slack token knows about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRef {
    pub key: String,
    pub kind: Kind,
    pub team: String,
    pub team_id: String,
    /// The user the token acts as (for a bot token, the bot user).
    pub account: String,
    pub url: String,
    /// Set when `slack login` minted the token, so a rotating one can refresh.
    pub client_id_key: Option<String>,
    pub client_secret_key: Option<String>,
}

/// Who a token belongs to, as `auth.test` reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    pub user: String,
    pub user_id: String,
    pub team: String,
    pub team_id: String,
    pub url: String,
    pub bot_id: Option<String>,
}

impl Identity {
    pub fn from_auth_test(v: &Value) -> Self {
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        Identity {
            user: s("user"),
            user_id: s("user_id"),
            team: s("team"),
            team_id: s("team_id"),
            url: s("url"),
            bot_id: v
                .get("bot_id")
                .and_then(|b| b.as_str())
                .filter(|b| !b.is_empty())
                .map(String::from),
        }
    }
}

pub(crate) fn tags_for(
    kind: Kind,
    who: &Identity,
    client_keys: Option<(&str, &str)>,
) -> Vec<String> {
    let mut tags = vec![
        "slack".to_string(),
        MARKER_TAG.to_string(),
        format!("{KIND_TAG}{}", kind.as_str()),
        format!("{TEAM_TAG}{}", who.team),
        format!("{TEAM_ID_TAG}{}", who.team_id),
        format!("{ACCOUNT_TAG}{}", who.user),
        format!("{URL_TAG}{}", who.url),
    ];
    if let Some((id, secret)) = client_keys {
        tags.push(format!("{CLIENT_ID_TAG}{id}"));
        tags.push(format!("{CLIENT_SECRET_TAG}{secret}"));
    }
    tags
}

/// Read a kv entry's tags back into a token reference, or `None` when the
/// entry is not a Slack token.
pub(crate) fn token_ref_from(key: &str, tags: &[String]) -> Option<TokenRef> {
    if !tags.iter().any(|t| t == MARKER_TAG) {
        return None;
    }
    let find = |p: &str| {
        tags.iter()
            .find_map(|t| t.strip_prefix(p))
            .map(String::from)
    };
    let kind = match find(KIND_TAG).as_deref() {
        Some("bot") => Kind::Bot,
        _ => Kind::User,
    };
    Some(TokenRef {
        key: key.to_string(),
        kind,
        team: find(TEAM_TAG).unwrap_or_default(),
        team_id: find(TEAM_ID_TAG).unwrap_or_default(),
        account: find(ACCOUNT_TAG).unwrap_or_default(),
        url: find(URL_TAG).unwrap_or_default(),
        client_id_key: find(CLIENT_ID_TAG),
        client_secret_key: find(CLIENT_SECRET_TAG),
    })
}

fn kv(key: &str) -> Result<Option<String>> {
    Ok(crate::broker::kv_get(key)?.map(|e| e.value))
}

/// Every stored Slack token.
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
    crate::broker::kv_set(DEFAULT_TOKEN_KEY, key, Some(&["slack".into()]))
}

/// Which stored token a command should use: `--token`, then the default, then
/// a sole token. Several with no default is an error rather than a guess —
/// posting into the wrong workspace cannot be taken back.
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
                "no Slack token stored under {k}; run `sidekar slack login --token {k} …` or \
                 `sidekar slack add --token {k}`"
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
            "no Slack token stored. Run `sidekar slack setup` for the steps, then \
             `sidekar slack login` (OAuth) or `sidekar slack add` (an existing token)."
        ),
        1 => Ok(stored.into_iter().next().unwrap()),
        _ => bail!(
            "several Slack tokens are stored ({}). Pass --token <KV_KEY>, or pick a default \
             with `sidekar slack use <KV_KEY>`.",
            stored
                .iter()
                .map(|t| t.key.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Forget a token. Only the local copy: the grant stays on Slack's side until
/// revoked, so say so rather than imply otherwise.
pub fn forget(token_key: &str) -> Result<()> {
    let _ = crate::broker::kv_delete(token_key);
    if default_token_key()?.as_deref() == Some(token_key) {
        let _ = crate::broker::kv_delete(DEFAULT_TOKEN_KEY);
    }
    Ok(())
}

/// The redirect `slack login` sends and listens for.
pub fn redirect_uri(port: u16) -> String {
    format!("http://localhost:{port}/callback")
}

/// The consent URL. A user token asks for `user_scope`, a bot token `scope`;
/// asking for both would mint two tokens and leave one unused.
pub(crate) fn authorize_url(
    client_id: &str,
    redirect: &str,
    state: &str,
    kind: Kind,
    team: Option<&str>,
) -> String {
    let (param, scopes) = match kind {
        Kind::User => ("user_scope", USER_SCOPES),
        Kind::Bot => ("scope", BOT_SCOPES),
    };
    let mut url = format!(
        "https://slack.com/oauth/v2/authorize?client_id={}&{param}={}&redirect_uri={}&state={}",
        urlencoding::encode(client_id),
        urlencoding::encode(&scopes.join(",")),
        urlencoding::encode(redirect),
        urlencoding::encode(state),
    );
    if let Some(t) = team {
        // Skips the workspace picker, so the grant cannot land on whichever
        // workspace the browser happened to be signed into.
        url.push_str(&format!("&team={}", urlencoding::encode(t)));
    }
    url
}

/// Pull the token we asked for out of an `oauth.v2.access` response.
///
/// The bot token is at the top level; the user token is under `authed_user`.
/// Each carries `refresh_token` and `expires_in` only when the app has token
/// rotation on.
pub(crate) fn token_from_exchange(v: &Value, kind: Kind, now: u64) -> Result<ExpiringToken> {
    let src = match kind {
        Kind::Bot => v,
        Kind::User => v.get("authed_user").unwrap_or(&Value::Null),
    };
    ExpiringToken::from_response(src, now).ok_or_else(|| {
        anyhow::anyhow!(
            "Slack granted no {} token. Check the app requests {} scopes under OAuth & \
             Permissions.",
            kind.as_str(),
            match kind {
                Kind::User => "User Token",
                Kind::Bot => "Bot Token",
            }
        )
    })
}

/// Options for [`login`].
pub struct LoginOptions<'a> {
    pub token_key: &'a str,
    pub client_id_key: &'a str,
    pub client_secret_key: &'a str,
    pub kind: Kind,
    pub team: Option<&'a str>,
    pub port: u16,
    pub open_browser: bool,
}

/// Run the OAuth consent flow and store the token under `token_key`.
pub async fn login(opts: LoginOptions<'_>) -> Result<Identity> {
    let client_id = kv(opts.client_id_key)?
        .ok_or_else(|| anyhow::anyhow!("{} is not in sidekar kv", opts.client_id_key))?;
    let client_secret = kv(opts.client_secret_key)?
        .ok_or_else(|| anyhow::anyhow!("{} is not in sidekar kv", opts.client_secret_key))?;

    let listeners = crate::oauth_loopback::bind_localhost(opts.port)?;
    let redirect = redirect_uri(opts.port);
    let state = crate::message::gen_msg_id();
    let url = authorize_url(&client_id, &redirect, &state, opts.kind, opts.team);

    println!("Open this URL to authorize:\n  {url}\n");
    println!(
        "Listening on {redirect} for up to {}s. That exact URL must be listed under the app's \
         OAuth & Permissions > Redirect URLs.",
        CONSENT_TIMEOUT.as_secs()
    );
    if opts.open_browser && !crate::oauth_loopback::open_in_browser(&url) {
        println!("(could not launch a browser here; use the URL above)");
    }

    let code = crate::oauth_loopback::wait_for_code(listeners, &state, "Slack", CONSENT_TIMEOUT)?;
    let res = exchange(
        &crate::http_client::client(),
        super::BASE,
        &[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", code.as_str()),
            ("redirect_uri", redirect.as_str()),
        ],
    )
    .await?;
    let token = token_from_exchange(&res, opts.kind, crate::oauth_loopback::now_secs())?;

    // Ask Slack who this is rather than trusting the exchange: auth.test is
    // the same call `doctor` makes, so a token that passes here works.
    let who = identify(&Slack::new(token.access_token.clone())).await?;
    crate::broker::kv_set(
        opts.token_key,
        &token.to_value(),
        Some(&tags_for(
            opts.kind,
            &who,
            Some((opts.client_id_key, opts.client_secret_key)),
        )),
    )?;
    if default_token_key()?.is_none() {
        set_default_token_key(opts.token_key)?;
    }
    Ok(who)
}

/// Adopt a token the user already put in kv under `token_key`.
pub async fn add(token_key: &str) -> Result<(Kind, Identity)> {
    let entry = crate::broker::kv_get(token_key)?.ok_or_else(|| {
        anyhow::anyhow!(
            "{token_key} is not in sidekar kv. Store the token first:\n  \
             sidekar kv set {token_key} 'xoxp-…'"
        )
    })?;
    let token = ExpiringToken::parse(&entry.value);
    let who = identify(&Slack::new(token.access_token.clone())).await?;
    let kind = Kind::from_token(&token.access_token).unwrap_or(if who.bot_id.is_some() {
        Kind::Bot
    } else {
        Kind::User
    });
    // Keep whatever tags the entry already had; add ours.
    let mut tags = entry.tags.clone();
    for t in tags_for(kind, &who, None) {
        if !tags.contains(&t) {
            tags.push(t);
        }
    }
    crate::broker::kv_set(token_key, &entry.value, Some(&tags))?;
    if default_token_key()?.is_none() {
        set_default_token_key(token_key)?;
    }
    Ok((kind, who))
}

pub async fn identify(slack: &Slack) -> Result<Identity> {
    Ok(Identity::from_auth_test(
        &slack.get("auth.test", &[]).await?,
    ))
}

/// `oauth.v2.access`, for both the code exchange and a refresh.
///
/// Form-encoded with the client secret in the body, as Slack documents. Not
/// sent through [`Slack::post`]: there is no bearer token yet.
pub(crate) async fn exchange(
    http: &reqwest::Client,
    base: &str,
    form: &[(&str, &str)],
) -> Result<Value> {
    let res = http
        .post(format!("{}/oauth.v2.access", base.trim_end_matches('/')))
        .form(form)
        .send()
        .await?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    super::check("oauth.v2.access", status, &text)
}

/// The access token to call with, refreshed first if it rotates and is near
/// expiry. A token without rotation is returned as stored.
pub async fn access_token_for(token: &TokenRef) -> Result<String> {
    let stored = kv(&token.key)?
        .ok_or_else(|| anyhow::anyhow!("no Slack token stored under {}", token.key))?;
    let current = ExpiringToken::parse(&stored);
    let now = crate::oauth_loopback::now_secs();
    if !current.needs_refresh(now) {
        return Ok(current.access_token);
    }
    let (Some(id_key), Some(secret_key)) = (&token.client_id_key, &token.client_secret_key) else {
        bail!(
            "the Slack token in {} has expired and was not minted by `slack login`, so there is \
             no client to refresh it with. Store a fresh one and run `sidekar slack add --token {}`.",
            token.key,
            token.key
        );
    };
    let client_id = kv(id_key)?.ok_or_else(|| anyhow::anyhow!("{id_key} is not in sidekar kv"))?;
    let client_secret =
        kv(secret_key)?.ok_or_else(|| anyhow::anyhow!("{secret_key} is not in sidekar kv"))?;
    let refresh = current.refresh_token.clone().unwrap_or_default();
    let res = exchange(
        &crate::http_client::client(),
        super::BASE,
        &[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
        ],
    )
    .await
    .map_err(|e| {
        anyhow::anyhow!(
            "could not refresh the Slack token in {}: {e}\nRe-authorize with `sidekar slack \
             login --token {} --client-id {id_key} --client-secret {secret_key}`.",
            token.key,
            token.key
        )
    })?;
    let next = refreshed(&res, &current, now)?;
    crate::broker::kv_set(&token.key, &next.to_value(), None)?;
    Ok(next.access_token)
}

/// The token after a refresh. A rotating refresh answers at the top level for
/// either kind; `authed_user` is checked too in case the shape follows the
/// original exchange. A response with no new refresh token keeps the old one.
pub(crate) fn refreshed(v: &Value, previous: &ExpiringToken, now: u64) -> Result<ExpiringToken> {
    let mut next = ExpiringToken::from_response(v, now)
        .or_else(|| {
            v.get("authed_user")
                .and_then(|u| ExpiringToken::from_response(u, now))
        })
        .ok_or_else(|| anyhow::anyhow!("Slack's refresh returned no access token"))?;
    if next.refresh_token.is_none() {
        next.refresh_token = previous.refresh_token.clone();
    }
    Ok(next)
}

/// The app manifest `slack setup` prints: paste it into "Create New App > From
/// an app manifest" and every scope and the redirect URL are already right.
pub fn manifest(app_name: &str, port: u16) -> Value {
    serde_json::json!({
        "display_information": { "name": app_name },
        "features": {
            "bot_user": { "display_name": app_name, "always_online": false }
        },
        "oauth_config": {
            "redirect_urls": [redirect_uri(port)],
            "scopes": { "user": USER_SCOPES, "bot": BOT_SCOPES }
        },
        "settings": {
            "org_deploy_enabled": false,
            "socket_mode_enabled": false,
            "token_rotation_enabled": false
        }
    })
}

#[cfg(test)]
mod tests;
