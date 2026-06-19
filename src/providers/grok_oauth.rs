//! Import Grok Build CLI OAuth sessions (`grok login`) for Sidekar REPL.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::oauth::{OAuthCredentials, kv_key_for, save_credentials};

pub const GROK_OAUTH_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
pub const GROK_OAUTH_DEFAULT_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const GROK_CLI_PROXY_BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";
const GROK_CLI_CLIENT_IDENTIFIER: &str = "xai-grok-cli";
const GROK_CLI_FALLBACK_VERSION: &str = "0.2.51";

/// True when REPL should talk to Grok Build's subscription proxy (not `api.x.ai`).
pub fn is_cli_proxy_base(base_url: &str) -> bool {
    base_url
        .trim_end_matches('/')
        .contains("cli-chat-proxy.grok.com")
}

/// Pick API root from stored credential metadata (`grok_cli_oauth` → proxy).
pub fn grok_repl_base_url(cred_name: &str) -> String {
    if credential_uses_cli_proxy(cred_name) {
        GROK_CLI_PROXY_BASE_URL.to_string()
    } else {
        super::oauth::GROK_BASE_URL.to_string()
    }
}

pub fn credential_uses_cli_proxy(cred_name: &str) -> bool {
    let kv_key = kv_key_for(cred_name);
    super::oauth::load_credentials(&kv_key)
        .ok()
        .flatten()
        .is_some_and(|c| {
            c.metadata
                .get("auth")
                .and_then(|v| v.as_str())
                == Some("grok_cli_oauth")
        })
}

/// Grok Build CLI version sent as `x-grok-client-version` (426 without it).
pub fn grok_cli_client_version() -> String {
    if let Ok(v) = std::env::var("GROK_CLI_CLIENT_VERSION")
        && !v.trim().is_empty()
    {
        return v.trim().to_string();
    }
    #[derive(Deserialize)]
    struct GrokVersionFile {
        #[serde(default)]
        version: String,
        #[serde(default)]
        stable_version: String,
    }
    let path = grok_home_dir().join("version.json");
    if let Ok(raw) = std::fs::read_to_string(&path)
        && let Ok(v) = serde_json::from_str::<GrokVersionFile>(&raw)
    {
        if !v.stable_version.is_empty() {
            return v.stable_version;
        }
        if !v.version.is_empty() {
            return v.version;
        }
    }
    GROK_CLI_FALLBACK_VERSION.to_string()
}

/// Required headers for `cli-chat-proxy.grok.com` (see Grok Build binary).
pub fn apply_cli_proxy_headers(headers: &mut reqwest::header::HeaderMap) {
    let version = grok_cli_client_version();
    let _ = headers.insert(
        "x-grok-client-version",
        version.parse().unwrap_or_else(|_| "0.2.51".parse().unwrap()),
    );
    let _ = headers.insert(
        "x-grok-client-identifier",
        GROK_CLI_CLIENT_IDENTIFIER
            .parse()
            .unwrap_or_else(|_| "xai-grok-cli".parse().unwrap()),
    );
    let _ = headers.insert(
        "X-XAI-Token-Auth",
        GROK_CLI_CLIENT_IDENTIFIER
            .parse()
            .unwrap_or_else(|_| "xai-grok-cli".parse().unwrap()),
    );
    let _ = headers.insert(
        "User-Agent",
        format!("xai-grok-workspace/{version}")
            .parse()
            .unwrap_or_else(|_| "xai-grok-workspace/0.2.51".parse().unwrap()),
    );
}

/// Grok Build CLI config root (`$GROK_HOME` or `~/.grok`).
pub fn grok_home_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("GROK_HOME")
        && !dir.trim().is_empty()
    {
        return expand_tilde(PathBuf::from(dir.trim()));
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".grok")
}

pub fn auth_json_path() -> PathBuf {
    grok_home_dir().join("auth.json")
}

#[derive(Debug, Clone)]
pub struct GrokCliSession {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub oidc_client_id: String,
    pub oidc_issuer: String,
    pub email: Option<String>,
}

/// Load the first usable OAuth session from Grok Build's `auth.json`.
pub fn load_cli_session() -> Option<GrokCliSession> {
    load_cli_session_from_path(&auth_json_path()).ok().flatten()
}

pub fn load_cli_session_from_path(path: &Path) -> Result<Option<GrokCliSession>> {
    if !path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let root: HashMap<String, GrokCliAuthEntry> =
        serde_json::from_str(&raw).context("failed to parse Grok auth.json")?;
    for entry in root.into_values() {
        let Some(session) = entry.into_session() else {
            continue;
        };
        return Ok(Some(session));
    }
    Ok(None)
}

pub fn save_imported_credential(nickname: &str, session: &GrokCliSession) -> Result<()> {
    let kv_key = kv_key_for(nickname);
    let creds = OAuthCredentials {
        access_token: session.access_token.clone(),
        refresh_token: session.refresh_token.clone(),
        expires_at: session.expires_at,
        metadata: serde_json::json!({
            "provider_type": "grok",
            "auth": "grok_cli_oauth",
            "oidc_client_id": session.oidc_client_id,
            "oidc_issuer": session.oidc_issuer,
            "email": session.email,
        }),
    };
    save_credentials(&kv_key, &creds)
}

pub fn oauth_client_id(creds: &OAuthCredentials) -> &str {
    creds
        .metadata
        .get("oidc_client_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(GROK_OAUTH_DEFAULT_CLIENT_ID)
}

pub fn oauth_token_url(creds: &OAuthCredentials) -> String {
    creds
        .metadata
        .get("oidc_issuer")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|issuer| format!("{}/oauth2/token", issuer.trim_end_matches('/')))
        .unwrap_or_else(|| GROK_OAUTH_TOKEN_URL.to_string())
}

pub fn credential_has_refresh_token(nickname: &str) -> bool {
    let kv_key = kv_key_for(nickname);
    super::oauth::load_credentials(&kv_key)
        .ok()
        .flatten()
        .is_some_and(|c| !c.refresh_token.is_empty())
}

/// Re-import `~/.grok/auth.json` when Sidekar's copy is expired but Grok CLI has fresher tokens.
pub fn sync_cli_session_from_disk_if_stale(cred_name: &str) -> Result<()> {
    if !credential_uses_cli_proxy(cred_name) {
        return Ok(());
    }
    let kv_key = kv_key_for(cred_name);
    let Some(stored) = super::oauth::load_credentials(&kv_key)? else {
        return Ok(());
    };
    if !stored.is_expired() {
        return Ok(());
    }
    let Some(session) = load_cli_session() else {
        return Ok(());
    };
    if session.expires_at <= stored.expires_at && session.access_token == stored.access_token {
        return Ok(());
    }
    save_imported_credential(cred_name, &session)
}

#[derive(Debug, Deserialize)]
struct GrokCliAuthEntry {
    #[serde(default)]
    key: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    expires_at: String,
    #[serde(default)]
    oidc_client_id: String,
    #[serde(default)]
    oidc_issuer: String,
    #[serde(default)]
    email: Option<String>,
}

impl GrokCliAuthEntry {
    fn into_session(self) -> Option<GrokCliSession> {
        if self.key.trim().is_empty() || self.refresh_token.trim().is_empty() {
            return None;
        }
        let oidc_client_id = if self.oidc_client_id.trim().is_empty() {
            GROK_OAUTH_DEFAULT_CLIENT_ID.to_string()
        } else {
            self.oidc_client_id
        };
        let oidc_issuer = if self.oidc_issuer.trim().is_empty() {
            "https://auth.x.ai".to_string()
        } else {
            self.oidc_issuer
        };
        let expires_at = jwt_exp_secs(&self.key)
            .or_else(|| parse_iso8601_utc_secs(&self.expires_at))
            .unwrap_or_else(|| now_secs().saturating_add(3600));
        Some(GrokCliSession {
            access_token: self.key,
            refresh_token: self.refresh_token,
            expires_at,
            oidc_client_id,
            oidc_issuer,
            email: self.email.filter(|e| !e.is_empty()),
        })
    }
}

fn expand_tilde(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if s == "~" {
        dirs::home_dir().unwrap_or(path)
    } else if let Some(rest) = s.strip_prefix("~/") {
        dirs::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or(path)
    } else {
        path
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn jwt_exp_secs(token: &str) -> Option<u64> {
    let payload = decode_jwt_payload(token)?;
    payload.get("exp").and_then(|v| v.as_u64())
}

fn decode_jwt_payload(token: &str) -> Option<serde_json::Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    use base64::Engine;
    let padded = match parts[1].len() % 4 {
        2 => format!("{}==", parts[1]),
        3 => format!("{}=", parts[1]),
        _ => parts[1].to_string(),
    };
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(padded.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Parse `2026-06-15T15:06:00.076745Z` (Grok auth.json `expires_at`).
fn parse_iso8601_utc_secs(raw: &str) -> Option<u64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let (y, rest) = date.split_once('-')?;
    let (mo, d) = rest.split_once('-')?;
    let time = time.split('.').next()?;
    let (h, rest) = time.split_once(':')?;
    let (mi, sec) = rest.split_once(':')?;
    let y: u64 = y.parse().ok()?;
    let mo: u64 = mo.parse().ok()?;
    let d: u64 = d.parse().ok()?;
    let h: u64 = h.parse().ok()?;
    let mi: u64 = mi.parse().ok()?;
    let sec: u64 = sec.parse().ok()?;
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    // Approximate UTC unix time — good enough for refresh scheduling; JWT `exp` is preferred.
    let month_days = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let mut days: u64 = (y - 1970) * 365 + (y - 1969) / 4 - (y - 1901) / 100 + (y - 1601) / 400;
    for m in 1..mo {
        days += month_days[m as usize] as u64;
        if m == 2 && leap {
            days += 1;
        }
    }
    days += d - 1;
    Some(days * 86_400 + h * 3600 + mi * 60 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_grok_expires_at_iso() {
        let secs = parse_iso8601_utc_secs("2026-06-15T15:06:00.076745Z").unwrap();
        assert!(secs > 1_780_000_000);
    }

    #[test]
    fn load_cli_session_from_fixture() {
        let dir = std::env::temp_dir().join(format!("sidekar-grok-oauth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("auth.json");
        std::fs::write(
            &path,
            r#"{
  "https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828": {
    "key": "access-token-value",
    "refresh_token": "refresh-token-value",
    "expires_at": "2026-06-15T15:06:00.076745Z",
    "oidc_client_id": "client-id",
    "oidc_issuer": "https://auth.x.ai",
    "email": "dev@example.com"
  }
}"#,
        )
        .unwrap();
        let session = load_cli_session_from_path(&path).unwrap().unwrap();
        assert_eq!(session.access_token, "access-token-value");
        assert_eq!(session.refresh_token, "refresh-token-value");
        assert_eq!(session.oidc_client_id, "client-id");
        assert_eq!(session.email.as_deref(), Some("dev@example.com"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_proxy_base_detection() {
        assert!(is_cli_proxy_base(GROK_CLI_PROXY_BASE_URL));
        assert!(!is_cli_proxy_base("https://api.x.ai"));
    }

    #[test]
    fn sync_cli_session_from_disk_if_stale_updates_expired_copy() {
        let dir = std::env::temp_dir().join(format!("sidekar-grok-sync-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let auth_path = dir.join("auth.json");
        std::fs::write(
            &auth_path,
            r#"{
  "https://auth.x.ai/": {
    "key": "eyJ0eXAiOiJhdCtqd3QiLCJhbGciOiJFUzI1NiIsImtpZCI6Im9hdXRoMi1wcm9kdWN0aW9uLTIwMjYtMDItMTkifQ.eyJleHAiOjE5OTk5OTk5OTl9.sig",
    "refresh_token": "refresh-new",
    "expires_at": "2099-01-01T00:00:00Z",
    "oidc_client_id": "b1a00492-073a-47ea-816f-4c329264a828",
    "oidc_issuer": "https://auth.x.ai",
    "email": "dev@example.com"
  }
}"#,
        )
        .unwrap();

        let nickname = format!("grok-sync-{}", std::process::id());
        let kv_key = crate::providers::oauth::kv_key_for(&nickname);
        let stale = crate::providers::oauth::OAuthCredentials {
            access_token: "old-token".to_string(),
            refresh_token: "refresh-old".to_string(),
            expires_at: 1,
            metadata: serde_json::json!({
                "provider_type": "grok",
                "auth": "grok_cli_oauth",
            }),
        };
        crate::providers::oauth::save_credentials(&kv_key, &stale).unwrap();

        let session = load_cli_session_from_path(&auth_path)
            .unwrap()
            .expect("fixture session");
        assert!(session.expires_at > stale.expires_at);

        unsafe {
            std::env::set_var("GROK_HOME", &dir);
        }
        sync_cli_session_from_disk_if_stale(&nickname).unwrap();
        let updated = crate::providers::oauth::load_credentials(&kv_key)
            .unwrap()
            .expect("updated creds");
        assert_eq!(updated.access_token, session.access_token);
        assert_eq!(updated.refresh_token, "refresh-new");
        assert!(updated.expires_at > stale.expires_at);

        let _ = crate::broker::kv_delete(&kv_key);
        let _ = std::fs::remove_dir_all(&dir);
        unsafe {
            std::env::remove_var("GROK_HOME");
        }
    }

    #[test]
    fn grok_cli_client_version_fallback() {
        assert!(!grok_cli_client_version().is_empty());
    }
}
