use super::*;
use crate::auth;

/// Token verification cache keyed by ext_token prefix (first 16 chars).
/// Avoids network call on every extension reconnect.
struct CacheEntry {
    user_id: String,
    expires_at: u64,
}

static TOKEN_CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, CacheEntry>>> =
    std::sync::OnceLock::new();

fn token_cache_key(ext_token: &str) -> String {
    ext_token.chars().take(16).collect()
}

fn get_cached_user_id(ext_token: &str) -> Option<String> {
    let map = TOKEN_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let map = map.lock().ok()?;
    let entry = map.get(&token_cache_key(ext_token))?;
    if entry.expires_at > epoch_secs() {
        Some(entry.user_id.clone())
    } else {
        None
    }
}

fn set_cached_user_id(ext_token: &str, user_id: String) {
    let map = TOKEN_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Ok(mut map) = map.lock() {
        // Evict expired entries
        let now = epoch_secs();
        map.retain(|_, v| v.expires_at > now);
        map.insert(
            token_cache_key(ext_token),
            CacheEntry {
                user_id,
                expires_at: now + 300,
            },
        );
    }
}

/// Verification outcome with structured error classification.
pub enum VerifyResult {
    /// Token verified, user_id returned.
    Ok(String),
    /// Token is definitively invalid — extension should clear it.
    InvalidToken(String),
    /// Transient/network error — extension should retry, NOT clear token.
    TransientError(String),
}

pub fn verify_ext_token(ext_token: &str) -> VerifyResult {
    // Try cache first
    if let Some(cached_user_id) = get_cached_user_id(ext_token) {
        return VerifyResult::Ok(cached_user_id);
    }

    let device_token = match auth::auth_token() {
        Some(t) => t,
        None => {
            return VerifyResult::TransientError(
                "CLI not logged in. Run: sidekar device login".into(),
            );
        }
    };

    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => return VerifyResult::TransientError(format!("HTTP client error: {e}")),
    };

    let url = format!("{}/api/auth/device?action=ext-verify", ext_api_base());
    let resp = match client
        .post(&url)
        .header("Authorization", format!("Bearer {}", device_token))
        .json(&json!({ "ext_token": ext_token }))
        .send()
    {
        Ok(r) => r,
        Err(e) => return VerifyResult::TransientError(format!("Cannot reach sidekar.dev: {e}")),
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        // 401 with "invalid ext token" or "invalid device token" = definitive
        if status.as_u16() == 401 {
            return VerifyResult::InvalidToken(format!("Token rejected by server ({status})"));
        }
        // Other HTTP errors are transient
        return VerifyResult::TransientError(format!("Server error: HTTP {status} — {body}"));
    }

    let data: Value = match resp.json() {
        Ok(d) => d,
        Err(e) => return VerifyResult::TransientError(format!("Invalid response: {e}")),
    };

    let matched = data.get("match").and_then(|v| v.as_bool()).unwrap_or(false);
    if !matched {
        return VerifyResult::InvalidToken(
            "Extension token and CLI token belong to different users".into(),
        );
    }

    let user_id = match data.get("user_id").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return VerifyResult::TransientError("No user_id in verification response".into()),
    };

    // Cache the result
    set_cached_user_id(ext_token, user_id.clone());

    VerifyResult::Ok(user_id)
}

/// Check if the extension is connected and authenticated (blocking, 500ms max).
pub fn is_ext_available() -> bool {
    if !crate::daemon::is_running() {
        return false;
    }
    crate::daemon::send_command(&json!({"type": "ext_status"}))
        .ok()
        .map(|val| {
            val.get("connected")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                && val
                    .get("authenticated")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
        })
        .unwrap_or(false)
}
