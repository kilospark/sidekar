//! The pieces every browser-consent login shares: a loopback listener that
//! catches the provider's redirect, and the stored shape of a token that
//! expires.
//!
//! Google, Slack and Linear all hand the authorization code back the same way
//! — a redirect to a port on this machine carrying `code` and `state` — so the
//! listener lives here once rather than three times. What differs is how each
//! provider matches the redirect URL it was given:
//!
//! - Google's Desktop clients accept any loopback port, so `google login`
//!   binds port 0 and lets the OS pick.
//! - Slack and Linear compare the redirect against URLs registered on the app,
//!   host and port included, so their logins listen on a fixed port the user
//!   registered once.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// How long to wait for the human to finish consenting in the browser.
pub(crate) const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

/// How long one connection gets to send its request line. The listener
/// serves one connection at a time, so without this a socket that connects
/// and says nothing (a browser preconnect, a port scanner) would hold it
/// forever and the consent timeout would never be checked again.
pub(crate) const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// The longest request line read. A redirect with a code is well under 2KB.
const MAX_REQUEST_LINE: usize = 16 * 1024;

/// Refresh this long before a token's stated expiry, so a command that starts
/// with a minute left does not fail halfway through.
const EXPIRY_SKEW_SECS: u64 = 300;

/// Listen on `localhost:<port>` for a provider that redirects to a fixed URL.
///
/// Both loopback families, because `localhost` in the registered URL resolves
/// to `::1` first on many machines and a browser that gets a refused
/// connection there does not always retry on `127.0.0.1`.
///
/// The IPv6 bind may be skipped only when this host has no IPv6 loopback. If
/// another process already holds `[::1]:<port>`, a browser that tries `::1`
/// first would hand *it* the authorization code, so that is an error.
pub(crate) fn bind_localhost(port: u16) -> Result<Vec<TcpListener>> {
    let busy = |addr: &str| {
        format!(
            "could not listen on {addr}:{port}. Something else holds the port; \
             pass --port <N> and register http://localhost:<N>/callback on the app"
        )
    };
    let v4 = TcpListener::bind(("127.0.0.1", port)).with_context(|| busy("127.0.0.1"))?;
    let mut out = vec![v4];
    match TcpListener::bind(("::1", port)) {
        Ok(v6) => out.push(v6),
        Err(e) if ipv6_absent(&e) => {}
        Err(e) => return Err(anyhow::Error::new(e).context(busy("[::1]"))),
    }
    Ok(out)
}

/// True for the errors that mean "no IPv6 loopback here", as opposed to
/// "someone else has the port".
pub(crate) fn ipv6_absent(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    if matches!(e.kind(), AddrNotAvailable | Unsupported) {
        return true;
    }
    #[cfg(unix)]
    {
        matches!(e.raw_os_error(), Some(code) if code == libc::EAFNOSUPPORT || code == libc::EADDRNOTAVAIL)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// An unguessable OAuth `state`: 128 random bits, hex.
pub(crate) fn random_state() -> String {
    let b: [u8; 16] = rand::random();
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A PKCE (RFC 7636) verifier and its S256 challenge. The verifier stays in
/// memory and goes only to the token endpoint, so a code caught by anything
/// else on this machine cannot be exchanged.
#[derive(Debug, Clone)]
pub(crate) struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> Self {
        use base64::Engine;
        let bytes: [u8; 32] = rand::random();
        let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        Self::from_verifier(verifier)
    }

    pub fn from_verifier(verifier: String) -> Self {
        use base64::Engine;
        use sha2::Digest;
        let digest = sha2::Sha256::digest(verifier.as_bytes());
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
        Self {
            verifier,
            challenge,
        }
    }

    /// The query parameters for the authorize URL.
    pub fn query(&self) -> String {
        format!(
            "&code_challenge={}&code_challenge_method=S256",
            self.challenge
        )
    }
}

/// Open a URL in the user's browser, best effort. The caller prints the URL
/// first regardless, because launching can no-op silently.
pub(crate) fn open_in_browser(url: &str) -> bool {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    matches!(
        std::process::Command::new(opener)
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
        Ok(st) if st.success()
    )
}

/// Block until the provider redirects back with our `state`, then hand the
/// browser something readable. `provider` only names the service in errors.
///
/// Only a request carrying the expected `state` ends the wait. Anything else
/// (favicon fetches, preconnects that never send, a page on this machine
/// probing the port with a made-up `state` or `error`) gets a 404 and the
/// listener keeps going until `timeout`, which is enforced on every pass.
pub(crate) fn wait_for_code(
    listeners: Vec<TcpListener>,
    expect_state: &str,
    provider: &str,
    timeout: Duration,
) -> Result<String> {
    for l in &listeners {
        l.set_nonblocking(true)?;
    }
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            bail!("timed out waiting for the browser to come back from {provider}");
        }
        let mut accepted = None;
        for l in &listeners {
            match l.accept() {
                Ok((stream, _)) => {
                    accepted = Some(stream);
                    break;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                // A connection reset before accept completed is the client's
                // problem, not a reason to abandon the login.
                Err(ref e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e.into()),
            }
        }
        let Some(mut stream) = accepted else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        let read_until = (Instant::now() + REQUEST_READ_TIMEOUT).min(deadline);
        let Some(request_line) = read_request_line(&mut stream, read_until) else {
            // Idle, too slow, too long, or closed early: not the redirect.
            continue;
        };
        let target = request_line.split_whitespace().nth(1).unwrap_or("/");
        let params = query_params(target);
        // The state check comes before anything else in the request is
        // believed, `error` included: without it any page in the browser
        // could cancel the login by redirecting here with `?error=…`.
        if params.get("state").map(String::as_str) != Some(expect_state) {
            respond(&mut stream, "404 Not Found", "");
            continue;
        }
        let ok = params.contains_key("code") && !params.contains_key("error");
        respond(
            &mut stream,
            "200 OK",
            if ok {
                "Signed in. You can close this tab and return to the terminal."
            } else {
                "Sign-in failed. Check the terminal."
            },
        );
        if let Some(err) = params.get("error") {
            bail!("{provider} refused the sign-in: {err}");
        }
        return params
            .get("code")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the redirect carried no authorization code"));
    }
}

/// The first line of an HTTP request, or `None` if it does not arrive whole
/// by `until`.
pub(crate) fn read_request_line(stream: &mut TcpStream, until: Instant) -> Option<String> {
    stream.set_nonblocking(false).ok()?;
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let left = until.checked_duration_since(Instant::now())?;
        if left.is_zero() {
            return None;
        }
        stream.set_read_timeout(Some(left)).ok()?;
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(end) = buf.iter().position(|&b| b == b'\n') {
                    return String::from_utf8(buf[..end].to_vec())
                        .ok()
                        .map(|l| l.trim_end_matches('\r').to_string());
                }
                if buf.len() > MAX_REQUEST_LINE {
                    return None;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let _ = stream.set_write_timeout(Some(REQUEST_READ_TIMEOUT));
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    );
    let _ = stream.flush();
}

pub(crate) fn query_params(target: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(q) = target.split_once('?').map(|(_, q)| q) else {
        return out;
    };
    for pair in q.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            let decoded = urlencoding::decode(&v.replace('+', " "))
                .map(|c| c.into_owned())
                .unwrap_or_default();
            out.insert(k.to_string(), decoded);
        }
    }
    out
}

/// An access token that expires, with what it takes to mint the next one.
///
/// Stored as JSON in the token's own kv entry. Linear's refresh tokens rotate
/// on every use, so unlike Google's the refresh token alone is not enough to
/// keep: the access token is cached until it nears expiry, which keeps
/// rotations — and kv writes, which sync — to about one a day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExpiringToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds. `None` means the provider gave no expiry.
    pub expires_at: Option<u64>,
}

impl ExpiringToken {
    /// A token response from any OAuth token endpoint, read at `now`.
    pub fn from_response(v: &Value, now: u64) -> Option<Self> {
        let access = v.get("access_token")?.as_str()?.to_string();
        if access.is_empty() {
            return None;
        }
        Some(Self {
            access_token: access,
            refresh_token: v
                .get("refresh_token")
                .and_then(|r| r.as_str())
                .filter(|r| !r.is_empty())
                .map(String::from),
            expires_at: v
                .get("expires_in")
                .and_then(|e| e.as_u64())
                .map(|secs| now + secs),
        })
    }

    /// What a kv value holds: this blob, or a bare token that never expires.
    pub fn parse(value: &str) -> Self {
        let trimmed = value.trim();
        if trimmed.starts_with('{')
            && let Ok(v) = serde_json::from_str::<Value>(trimmed)
            && let Some(access) = v.get("access_token").and_then(|a| a.as_str())
        {
            return Self {
                access_token: access.to_string(),
                refresh_token: v
                    .get("refresh_token")
                    .and_then(|r| r.as_str())
                    .map(String::from),
                expires_at: v.get("expires_at").and_then(|e| e.as_u64()),
            };
        }
        Self {
            access_token: trimmed.to_string(),
            refresh_token: None,
            expires_at: None,
        }
    }

    /// The kv value for this token. A token with nothing to refresh is stored
    /// bare, so a value someone pasted in by hand round-trips unchanged.
    pub fn to_value(&self) -> String {
        if self.refresh_token.is_none() && self.expires_at.is_none() {
            return self.access_token.clone();
        }
        json!({
            "access_token": self.access_token,
            "refresh_token": self.refresh_token,
            "expires_at": self.expires_at,
        })
        .to_string()
    }

    /// True when the access token is expired or about to be, and there is a
    /// refresh token to replace it with.
    pub fn needs_refresh(&self, now: u64) -> bool {
        self.refresh_token.is_some()
            && self
                .expires_at
                .is_some_and(|at| now + EXPIRY_SKEW_SECS >= at)
    }
}

/// How long to wait for another process that is refreshing the same token.
const REFRESH_LOCK_WAIT: Duration = Duration::from_secs(60);

/// An exclusive lock on one token key, across processes on this machine.
/// Released when dropped.
pub(crate) struct TokenLock {
    _file: std::fs::File,
}

fn lock_path(key: &str) -> std::path::PathBuf {
    let safe: String = key
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
        .join("locks")
        .join(format!("token-{safe}.lock"))
}

pub(crate) async fn lock_token(key: &str) -> Result<TokenLock> {
    use fs2::FileExt;
    let path = lock_path(key);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("could not open {}", path.display()))?;
    let deadline = Instant::now() + REFRESH_LOCK_WAIT;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(TokenLock { _file: file }),
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => bail!(
                "another sidekar process has been refreshing {key} for over {}s ({e}); try again",
                REFRESH_LOCK_WAIT.as_secs()
            ),
        }
    }
}

fn stored_token(key: &str, provider: &str) -> Result<ExpiringToken> {
    crate::broker::kv_get(key)?
        .map(|e| ExpiringToken::parse(&e.value))
        .ok_or_else(|| anyhow::anyhow!("no {provider} token stored under {key}"))
}

/// Write a refreshed token back. The replaced value holds a refresh token
/// that is now spent; it is dropped from kv history rather than kept as a
/// copy of a credential.
fn store_token(key: &str, next: &ExpiringToken) -> Result<()> {
    crate::broker::kv_set(key, &next.to_value(), None)?;
    crate::broker::kv_clear_history(key)?;
    Ok(())
}

/// The token stored under `key`, refreshed through `refresh` if it is near
/// expiry: the read, the refresh and the write-back happen under one lock.
///
/// Rotating refresh tokens (Linear always, Slack with rotation on) are
/// single-use. Two processes that both read the old blob and both refresh
/// would spend the same refresh token twice, and the loser's user would have
/// to log in again. So:
///
/// 1. Under the lock, kv is read again; if another process already
///    refreshed, its token is used and nothing is sent.
/// 2. If the refresh fails, kv is read once more before the error is shown:
///    another machine may have rotated the token and sync delivered it while
///    this request was in flight. A newer valid token is used as is; a newer
///    refresh token gets one more try.
pub(crate) async fn refresh_stored<F, Fut>(
    key: &str,
    provider: &str,
    refresh: F,
) -> Result<ExpiringToken>
where
    F: Fn(ExpiringToken) -> Fut,
    Fut: std::future::Future<Output = Result<ExpiringToken>>,
{
    let current = stored_token(key, provider)?;
    if !current.needs_refresh(now_secs()) {
        return Ok(current);
    }
    let _lock = lock_token(key).await?;
    let current = stored_token(key, provider)?;
    if !current.needs_refresh(now_secs()) {
        return Ok(current);
    }
    match refresh(current.clone()).await {
        Ok(next) => {
            store_token(key, &next)?;
            Ok(next)
        }
        Err(e) => {
            let latest = stored_token(key, provider)?;
            if latest != current {
                if !latest.needs_refresh(now_secs()) {
                    return Ok(latest);
                }
                if latest.refresh_token != current.refresh_token
                    && let Ok(next) = refresh(latest).await
                {
                    store_token(key, &next)?;
                    return Ok(next);
                }
            }
            Err(e)
        }
    }
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
