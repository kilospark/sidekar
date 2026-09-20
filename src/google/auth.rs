//! Google OAuth, keyed by account so more than one can be signed in at once.
//!
//! Storage is a fixed pattern rather than fixed keys. Fixed keys meant a second
//! `google login` silently overwrote the first, which is a quiet way to lose
//! access to a mailbox nobody realised was being replaced.
//!
//! ```text
//! GOOGLE_OAUTH_CLIENT_ID__<DOMAIN>       one OAuth client per domain
//! GOOGLE_OAUTH_CLIENT_SECRET__<DOMAIN>
//! GOOGLE_REFRESH_TOKEN__<ACCOUNT>        one token per signed-in address
//! GOOGLE_DEFAULT_ACCOUNT                 which address commands use by default
//! ```
//!
//! The domain is derived from the address, so an account always knows which
//! client minted its token without a separate mapping to drift out of step.
//! That matters because a client is not universal: an Internal Workspace client
//! refuses every address outside its organisation with `403 org_internal`, so a
//! second domain needs a second client rather than a second login.
//!
//! Only the refresh token is stored. Access tokens last an hour and are cheap to
//! mint, so keeping them would add a second thing to leak for no benefit.

use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

/// Key holding the token key to use when none is named.
const DEFAULT_TOKEN_KEY: &str = "GOOGLE_DEFAULT_TOKEN";

/// Tags recorded on a token entry so it knows which client minted it.
const CLIENT_ID_TAG: &str = "client-id:";
const CLIENT_SECRET_TAG: &str = "client-secret:";
const ACCOUNT_TAG: &str = "acct:";

/// How long to wait for the human to finish consenting in the browser.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

/// Everything the Gmail, Drive, Calendar, Sheets and Docs commands need.
///
/// `gmail.modify` rather than full `mail.google.com`: it reads, sends and
/// labels, but cannot permanently delete, which is the one Gmail action with no
/// undo.
pub const SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/gmail.modify",
    "https://www.googleapis.com/auth/drive",
    "https://www.googleapis.com/auth/calendar",
    "https://www.googleapis.com/auth/spreadsheets",
    "https://www.googleapis.com/auth/documents",
    "https://www.googleapis.com/auth/userinfo.email",
];

fn kv(key: &str) -> Result<Option<String>> {
    Ok(crate::broker::kv_get(key)?.map(|e| e.value))
}

/// What a stored refresh token knows about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRef {
    /// KV key holding the refresh token.
    pub key: String,
    /// KV key holding the OAuth client id this token was minted with.
    pub client_id_key: String,
    /// KV key holding that client's secret.
    pub client_secret_key: String,
    /// The address that consented, as Google reported it.
    pub account: String,
}

pub(crate) fn tags_for(client_id_key: &str, client_secret_key: &str, account: &str) -> Vec<String> {
    vec![
        "google".to_string(),
        "oauth".to_string(),
        format!("{CLIENT_ID_TAG}{client_id_key}"),
        format!("{CLIENT_SECRET_TAG}{client_secret_key}"),
        format!("{ACCOUNT_TAG}{account}"),
    ]
}

/// Read a token entry's tags back into the client keys it was minted with.
///
/// The client keys travel with the token rather than being derived from the
/// account, because there is no rule connecting the two: a Workspace client is
/// Internal and refuses outside addresses, a personal account needs its own
/// External client, and one client can serve many accounts. Deriving would be a
/// guess; recording is a fact.
pub(crate) fn token_ref_from(key: &str, tags: &[String]) -> Option<TokenRef> {
    let find = |p: &str| {
        tags.iter()
            .find_map(|t| t.strip_prefix(p))
            .map(String::from)
    };
    Some(TokenRef {
        key: key.to_string(),
        client_id_key: find(CLIENT_ID_TAG)?,
        client_secret_key: find(CLIENT_SECRET_TAG)?,
        account: find(ACCOUNT_TAG).unwrap_or_default(),
    })
}

/// Every stored Google token, by the key it lives under.
pub fn tokens() -> Result<Vec<TokenRef>> {
    let mut out: Vec<TokenRef> = crate::broker::kv_list(None)?
        .into_iter()
        .filter_map(|e| token_ref_from(&e.key, &e.tags))
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

pub fn default_token_key() -> Result<Option<String>> {
    kv(DEFAULT_TOKEN_KEY)
}

pub fn set_default_token_key(key: &str) -> Result<()> {
    crate::broker::kv_set(
        DEFAULT_TOKEN_KEY,
        key,
        Some(&["google".into(), "oauth".into()]),
    )
}

/// Which stored token a command should use.
///
/// An explicit `--token` wins, then the recorded default, then a sole stored
/// token. Several with no default is an error rather than a guess: acting on the
/// wrong mailbox is not something the caller can undo by noticing later.
pub fn resolve_token(requested: Option<&str>) -> Result<TokenRef> {
    let stored = tokens()?;
    if let Some(k) = requested {
        return stored
            .into_iter()
            .find(|t| t.key == k)
            .ok_or_else(|| anyhow::anyhow!("no Google token stored under {k}; run `sidekar google login --token {k} --client-id <KEY> --client-secret <KEY>`"));
    }
    if let Some(d) = default_token_key()?
        && let Some(found) = stored.iter().find(|t| t.key == d)
    {
        return Ok(found.clone());
    }
    match stored.len() {
        0 => bail!(
            "no Google token stored. Run `sidekar google login --token <KV_KEY> \
             --client-id <KV_KEY> --client-secret <KV_KEY>`."
        ),
        1 => Ok(stored.into_iter().next().unwrap()),
        _ => bail!(
            "several Google tokens are stored ({}). Pass --token <KV_KEY>, or pick a default \
             with `sidekar google use <KV_KEY>`.",
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

/// Run the consent flow and store the refresh token under `token_key`.
///
/// The caller names every key. Sidekar invents no naming scheme, so credentials
/// that already exist under someone else's convention work unchanged.
pub async fn login(
    token_key: &str,
    client_id_key: &str,
    client_secret_key: &str,
    account_hint: Option<&str>,
    open_browser: bool,
) -> Result<String> {
    let client_id = kv(client_id_key)?
        .ok_or_else(|| anyhow::anyhow!("{client_id_key} is not in sidekar kv"))?;
    let client_secret = kv(client_secret_key)?
        .ok_or_else(|| anyhow::anyhow!("{client_secret_key} is not in sidekar kv"))?;

    // Port 0: let the OS pick. A Desktop-app client accepts any loopback port,
    // so nothing has to be pre-registered and two logins cannot collide.
    let listener = TcpListener::bind("127.0.0.1:0").context("could not open a loopback port")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}");
    let state = crate::message::gen_msg_id();

    let mut auth_url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code\
         &scope={}&access_type=offline&prompt=consent&state={}",
        urlencoding::encode(&client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(&SCOPES.join(" ")),
        urlencoding::encode(&state),
    );
    if let Some(a) = account_hint {
        // Skips the chooser, so a login cannot quietly land on whichever account
        // the browser happened to have active.
        auth_url.push_str(&format!("&login_hint={}", urlencoding::encode(a)));
    }

    // URL first, always. A caller relaying it to a human needs it before any
    // browser is involved, and `open` can no-op silently when the browser is not
    // where macOS expects it, which reads as a hang rather than a failure.
    println!("Open this URL to authorize:\n  {auth_url}\n");
    println!(
        "Listening on 127.0.0.1:{port} for up to {}s.",
        CONSENT_TIMEOUT.as_secs()
    );
    if open_browser {
        match std::process::Command::new("open").arg(&auth_url).status() {
            Ok(st) if st.success() => {}
            _ => println!("(could not launch a browser here; use the URL above)"),
        }
    }

    let code = wait_for_code(listener, &state)?;
    let tokens = exchange_code(&client_id, &client_secret, &code, &redirect_uri).await?;

    let refresh = tokens
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Google returned no refresh token. That happens when the account has already \
                 consented; revoke this app at myaccount.google.com/permissions and log in again."
            )
        })?;
    let access = tokens
        .get("access_token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    // Ask Google who consented rather than trusting the hint: the human can pick
    // a different account in the chooser, and recording the requested address
    // would label the token with a mailbox it does not open.
    let account = fetch_email(access).await.unwrap_or_default();
    crate::broker::kv_set(
        token_key,
        refresh,
        Some(&tags_for(client_id_key, client_secret_key, &account)),
    )?;
    if default_token_key()?.is_none() {
        set_default_token_key(token_key)?;
    }
    Ok(account)
}

/// Block until Google redirects back, then hand the browser something readable.
fn wait_for_code(listener: TcpListener, expect_state: &str) -> Result<String> {
    listener.set_nonblocking(true)?;
    let started = Instant::now();
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false)?;
                let mut reader = BufReader::new(stream.try_clone()?);
                let mut request_line = String::new();
                reader.read_line(&mut request_line)?;

                let target = request_line.split_whitespace().nth(1).unwrap_or("/");
                let params = query_params(target);
                let body = if params.contains_key("code") {
                    "Signed in. You can close this tab and return to the terminal."
                } else {
                    "Sign-in failed. Check the terminal."
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                         Connection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                );
                let _ = stream.flush();

                if let Some(err) = params.get("error") {
                    bail!("Google refused the sign-in: {err}");
                }
                // The state check is what stops another page on this machine from
                // feeding us a code for an account nobody asked to connect.
                match params.get("state") {
                    Some(s) if s == expect_state => {}
                    _ => bail!("the redirect carried the wrong state; sign-in abandoned"),
                }
                return params
                    .get("code")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("the redirect carried no authorization code"));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if started.elapsed() >= CONSENT_TIMEOUT {
                    bail!("timed out waiting for the browser to come back");
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn query_params(target: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Some(q) = target.split_once('?').map(|(_, q)| q) else {
        return out;
    };
    for pair in q.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            let decoded = urlencoding::decode(v)
                .map(|c| c.into_owned())
                .unwrap_or_default();
            out.insert(k.to_string(), decoded);
        }
    }
    out
}

async fn exchange_code(
    client_id: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
) -> Result<serde_json::Value> {
    let res = reqwest::Client::new()
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("code", code),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("redirect_uri", redirect_uri),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await?;
    let json: serde_json::Value = res.json().await?;
    if let Some(err) = json.get("error") {
        bail!("token exchange failed: {err}");
    }
    Ok(json)
}

/// A fresh access token, minted from the stored refresh token.
pub async fn access_token_for(token: &TokenRef) -> Result<String> {
    let refresh = kv(&token.key)?
        .ok_or_else(|| anyhow::anyhow!("no Google token stored under {}", token.key))?;
    let client_id = kv(&token.client_id_key)?
        .ok_or_else(|| anyhow::anyhow!("{} is not in sidekar kv", token.client_id_key))?;
    let client_secret = kv(&token.client_secret_key)?
        .ok_or_else(|| anyhow::anyhow!("{} is not in sidekar kv", token.client_secret_key))?;
    let res = reqwest::Client::new()
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("refresh_token", refresh.as_str()),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await?;
    let json: serde_json::Value = res.json().await?;
    if let Some(err) = json.get("error") {
        bail!("{}", explain_refresh_failure(token, err));
    }
    json.get("access_token")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("Google returned no access token"))
}

/// Turn Google's terse refresh errors into something actionable.
///
/// `invalid_grant` is almost always the seven-day expiry that Google applies to
/// External apps still in Testing, and it arrives with no explanation at all.
/// Left raw it reads as a broken token and sends the reader looking for a bug
/// that is not there.
pub(crate) fn explain_refresh_failure(token: &TokenRef, err: &serde_json::Value) -> String {
    let code = err.as_str().unwrap_or_default();
    let relogin = format!(
        "sidekar google login --token {} --client-id {} --client-secret {}",
        token.key, token.client_id_key, token.client_secret_key
    );
    if code == "invalid_grant" {
        return format!(
            "the Google token in {} is no longer valid.\n\n\
             The usual cause is the seven-day limit Google puts on refresh tokens for External \
             apps whose publishing status is still Testing. It is not a bug and nothing is \
             misconfigured; the grant simply expires on a timer. Publishing the app removes the \
             limit. Other causes are the account revoking access, or a password change when \
             Gmail scopes are involved.\n\n\
             Re-authorize with:\n  {relogin}",
            token.key
        );
    }
    format!(
        "could not refresh {} ({code}). Re-authorize with:\n  {relogin}",
        token.key
    )
}

async fn fetch_email(access: &str) -> Result<String> {
    let json: serde_json::Value = reqwest::Client::new()
        .get("https://www.googleapis.com/oauth2/v2/userinfo")
        .bearer_auth(access)
        .send()
        .await?
        .json()
        .await?;
    Ok(json
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string())
}

#[cfg(test)]
mod tests;
