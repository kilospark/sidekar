//! OAuth for the Kilo Spark Workspace, via a loopback redirect.
//!
//! The client is an Internal app in a Workspace project, which is what makes the
//! restricted Gmail and Drive scopes usable at all: Internal apps skip Google's
//! verification and CASA review entirely. It is a Desktop-app client, so Google
//! accepts a `http://127.0.0.1:<port>` redirect chosen at runtime.
//!
//! Only the refresh token is stored. Access tokens last an hour and are cheap to
//! mint, so keeping them would add a second thing to leak for no benefit.

use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

const CLIENT_ID_KEY: &str = "GOOGLE_KILOSPARK_OAUTH_CLIENT_ID";
const CLIENT_SECRET_KEY: &str = "GOOGLE_KILOSPARK_OAUTH_CLIENT_SECRET";
const REFRESH_TOKEN_KEY: &str = "GOOGLE_KILOSPARK_REFRESH_TOKEN";
const ACCOUNT_KEY: &str = "GOOGLE_KILOSPARK_ACCOUNT";

/// How long to wait for the human to finish consenting in the browser.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

/// Everything the Gmail, Drive and Calendar commands need.
///
/// `gmail.modify` rather than full `mail.google.com`: it can read, send, and
/// label, but cannot permanently delete, which is the one Gmail action with no
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

fn require_kv(key: &str) -> Result<String> {
    kv(key)?.ok_or_else(|| {
        anyhow::anyhow!("{key} is not in sidekar kv; the OAuth client has not been set up")
    })
}

pub fn client_credentials() -> Result<(String, String)> {
    Ok((require_kv(CLIENT_ID_KEY)?, require_kv(CLIENT_SECRET_KEY)?))
}

pub fn logged_in_account() -> Result<Option<String>> {
    kv(ACCOUNT_KEY)
}

pub fn is_logged_in() -> bool {
    kv(REFRESH_TOKEN_KEY).ok().flatten().is_some()
}

pub fn forget() -> Result<()> {
    let _ = crate::broker::kv_delete(REFRESH_TOKEN_KEY);
    let _ = crate::broker::kv_delete(ACCOUNT_KEY);
    Ok(())
}

/// Run the consent flow and store the refresh token.
///
/// Returns the account that consented.
pub async fn login() -> Result<String> {
    let (client_id, client_secret) = client_credentials()?;

    // Port 0: let the OS pick. A Desktop-app client accepts any loopback port,
    // so nothing has to be pre-registered and two logins cannot collide.
    let listener = TcpListener::bind("127.0.0.1:0").context("could not open a loopback port")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}");
    let state = crate::message::gen_msg_id();

    let auth_url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code\
         &scope={}&access_type=offline&prompt=consent&state={}",
        urlencoding::encode(&client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(&SCOPES.join(" ")),
        urlencoding::encode(&state),
    );

    println!("Opening your browser to sign in to Google.");
    println!("If it does not open, visit:\n  {auth_url}\n");
    let _ = std::process::Command::new("open").arg(&auth_url).status();

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

    let account = fetch_email(access).await.unwrap_or_default();
    crate::broker::kv_set(
        REFRESH_TOKEN_KEY,
        refresh,
        Some(&["google".into(), "kilospark".into(), "oauth".into()]),
    )?;
    if !account.is_empty() {
        crate::broker::kv_set(
            ACCOUNT_KEY,
            &account,
            Some(&["google".into(), "kilospark".into()]),
        )?;
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
pub async fn access_token() -> Result<String> {
    let refresh = kv(REFRESH_TOKEN_KEY)?.ok_or_else(|| {
        anyhow::anyhow!("not signed in to Google; run `sidekar google login` first")
    })?;
    let (client_id, client_secret) = client_credentials()?;
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
        bail!(
            "could not refresh the Google token ({err}); run `sidekar google login` to reconnect"
        );
    }
    json.get("access_token")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("Google returned no access token"))
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
