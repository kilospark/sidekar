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
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

/// How long to wait for the human to finish consenting in the browser.
pub(crate) const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

/// Refresh this long before a token's stated expiry, so a command that starts
/// with a minute left does not fail halfway through.
const EXPIRY_SKEW_SECS: u64 = 300;

/// Listen on `localhost:<port>` for a provider that redirects to a fixed URL.
///
/// Both loopback families, because `localhost` in the registered URL resolves
/// to `::1` first on many machines and a browser that gets a refused
/// connection there does not always retry on `127.0.0.1`. The IPv6 bind is
/// best effort: a host without IPv6 still has the IPv4 one.
pub(crate) fn bind_localhost(port: u16) -> Result<Vec<TcpListener>> {
    let v4 = TcpListener::bind(("127.0.0.1", port)).with_context(|| {
        format!(
            "could not listen on 127.0.0.1:{port}. Something else holds the port; \
             pass --port <N> and register http://localhost:<N>/callback on the app"
        )
    })?;
    let mut out = vec![v4];
    if let Ok(v6) = TcpListener::bind(("::1", port)) {
        out.push(v6);
    }
    Ok(out)
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

/// Block until the provider redirects back, then hand the browser something
/// readable. `provider` only names the service in errors.
pub(crate) fn wait_for_code(
    listeners: Vec<TcpListener>,
    expect_state: &str,
    provider: &str,
    timeout: Duration,
) -> Result<String> {
    for l in &listeners {
        l.set_nonblocking(true)?;
    }
    let started = Instant::now();
    loop {
        let mut accepted = None;
        for l in &listeners {
            match l.accept() {
                Ok((stream, _)) => {
                    accepted = Some(stream);
                    break;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
        }
        let Some(mut stream) = accepted else {
            if started.elapsed() >= timeout {
                bail!("timed out waiting for the browser to come back from {provider}");
            }
            std::thread::sleep(Duration::from_millis(200));
            continue;
        };

        stream.set_nonblocking(false)?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut request_line = String::new();
        reader.read_line(&mut request_line)?;

        let target = request_line.split_whitespace().nth(1).unwrap_or("/");
        let params = query_params(target);
        // Browsers open speculative connections and ask for /favicon.ico;
        // neither carries the redirect, so keep waiting for the one that does.
        if !params.contains_key("state") && !params.contains_key("error") {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            continue;
        }
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
            bail!("{provider} refused the sign-in: {err}");
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

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
