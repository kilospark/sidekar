//! PKCE OAuth 2.0 flow for Anthropic Claude and OpenAI Codex subscriptions.
//!
//! Tokens are stored encrypted in sidekar's KV store.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::Write;

// ---------------------------------------------------------------------------
// Provider configs (from pi-mono)
// ---------------------------------------------------------------------------

const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const ANTHROPIC_AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";
const ANTHROPIC_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const ANTHROPIC_CALLBACK_PORT: u16 = 53692;
const ANTHROPIC_SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_CALLBACK_PORT: u16 = 1455;
const CODEX_SCOPES: &str = "openid profile email offline_access";

pub const KV_KEY_ANTHROPIC: &str = "oauth:anthropic";
pub const KV_KEY_CODEX: &str = "oauth:codex";
pub const KV_KEY_OPENROUTER: &str = "oauth:openrouter";
pub const KV_KEY_OPENCODE: &str = "oauth:opencode";
pub const KV_KEY_GROK: &str = "oauth:grok";
pub const GROK_BASE_URL: &str = "https://api.x.ai";

/// KV key for a named credential. e.g., "claude-1" → "oauth:claude-1"
pub fn kv_key_for(nickname: &str) -> String {
    format!("oauth:{nickname}")
}

/// Resolve which KV key to use.
fn resolve_kv_key(nickname: Option<&str>, default_key: &str) -> String {
    match nickname {
        Some(n) => kv_key_for(n),
        None => default_key.to_string(),
    }
}

/// Provider type for a nickname.
///
/// A nickname matches a convention only if it is exactly the prefix
/// (`claude`) or uses a dash boundary (`claude-work`). This prevents names
/// like `oracle-prod` from being misclassified as OpenRouter and `ocean`
/// from being misclassified as OpenCode.
pub fn provider_type_for(nickname: &str) -> Option<&'static str> {
    if matches_convention(nickname, "claude") {
        Some("anthropic")
    } else if matches_convention(nickname, "codex") {
        Some("codex")
    } else if matches_convention(nickname, "or") {
        Some("openrouter")
    } else if matches_convention(nickname, "oc") || matches_convention(nickname, "opencode") {
        Some("opencode")
    } else if matches_convention(nickname, "grok") {
        Some("grok")
    } else if matches_convention(nickname, "oac") {
        Some("oac")
    } else {
        stored_provider_type_for(nickname)
    }
}

fn matches_convention(nickname: &str, prefix: &str) -> bool {
    nickname == prefix
        || (nickname.starts_with(prefix) && nickname.as_bytes().get(prefix.len()) == Some(&b'-'))
}

fn stored_provider_type_for(nickname: &str) -> Option<&'static str> {
    let key = kv_key_for(nickname);
    let creds = load_credentials(&key).ok()??;
    match creds.metadata.get("provider_type").and_then(|v| v.as_str()) {
        Some("anthropic") => Some("anthropic"),
        Some("codex") => Some("codex"),
        Some("openrouter") => Some("openrouter"),
        Some("opencode") => Some("opencode"),
        Some("grok") => Some("grok"),
        Some("oac") => Some("oac"),
        Some("openai-compatible") => Some("oac"),
        _ => None,
    }
}

/// Get the email/identity stored in a credential's metadata.
pub fn credential_email(nickname: &str) -> Option<String> {
    let key = format!("oauth:{nickname}");
    let entry = crate::broker::kv_get(&key).ok()??;
    let creds: OAuthCredentials = serde_json::from_str(&entry.value).ok()?;
    let email = creds
        .metadata
        .get("email")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    if email.is_some() {
        return email;
    }
    // Fallback to name
    creds
        .metadata
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// List all stored credential nicknames.
pub fn list_credentials() -> Vec<(String, String)> {
    let entries = crate::broker::kv_list(None).unwrap_or_default();
    entries
        .into_iter()
        .filter_map(|e| {
            let name = e.key.strip_prefix("oauth:")?;
            let provider = provider_type_for(name).unwrap_or(if name == "anthropic" {
                "anthropic"
            } else if name == "codex" {
                "codex"
            } else if name == "openrouter" {
                "openrouter"
            } else if name == "opencode" {
                "opencode"
            } else if name == "grok" {
                "grok"
            } else {
                "unknown"
            });
            Some((name.to_string(), provider.to_string()))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Stored credentials
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCredentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl OAuthCredentials {
    fn is_expired(&self) -> bool {
        let now = now_secs();
        now + 300 >= self.expires_at
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Force-refresh the stored access token for `cred_name` using its refresh_token,
/// regardless of the cached `expires_at`. Called after a 401 from the provider —
/// the server rejected a token we thought was still valid (clock drift,
/// early rotation, or revocation). Anthropic and Codex only; other providers
/// store static API keys with no refresh path.
pub async fn force_refresh_token(cred_name: &str) -> Result<String> {
    let provider_type = provider_type_for(cred_name)
        .ok_or_else(|| anyhow::anyhow!("unknown credential '{cred_name}'"))?;

    let (kv_key, refresh_fn): (
        String,
        fn(
            &OAuthCredentials,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<OAuthCredentials>> + Send>>,
    ) = match provider_type {
        "anthropic" => (
            resolve_kv_key(Some(cred_name), KV_KEY_ANTHROPIC),
            refresh_token_anthropic,
        ),
        "codex" => (
            resolve_kv_key(Some(cred_name), KV_KEY_CODEX),
            refresh_token_codex,
        ),
        other => anyhow::bail!("provider '{other}' has no refresh flow — re-authenticate via `sidekar repl login {cred_name}`"),
    };

    let creds = load_credentials(&kv_key)?
        .ok_or_else(|| anyhow::anyhow!("no stored credentials for '{cred_name}'"))?;
    if creds.refresh_token.is_empty() {
        anyhow::bail!(
            "credential '{cred_name}' has no refresh token — re-authenticate via `sidekar repl login {cred_name}`"
        );
    }
    let new_creds = refresh_fn(&creds).await?;
    save_credentials(&kv_key, &new_creds)?;
    Ok(new_creds.access_token)
}

/// Get a valid Anthropic API token. If `nickname` is provided, use that credential set.
pub async fn get_anthropic_token(nickname: Option<&str>) -> Result<String> {
    let kv_key = resolve_kv_key(nickname, KV_KEY_ANTHROPIC);
    get_token(
        &kv_key,
        "ANTHROPIC_API_KEY",
        "Anthropic",
        anthropic_login,
        refresh_token_anthropic,
        false,
    )
    .await
}

/// Get a valid Anthropic API token, with interactive login if needed.
pub async fn login_anthropic(nickname: Option<&str>) -> Result<String> {
    let kv_key = resolve_kv_key(nickname, KV_KEY_ANTHROPIC);
    get_token(
        &kv_key,
        "ANTHROPIC_API_KEY",
        "Anthropic",
        anthropic_login,
        refresh_token_anthropic,
        true,
    )
    .await
}

/// Get a valid Codex API token. If `nickname` is provided, use that credential set.
pub async fn get_codex_token(nickname: Option<&str>) -> Result<(String, String)> {
    let kv_key = resolve_kv_key(nickname, KV_KEY_CODEX);
    let token = get_token(
        &kv_key,
        "OPENAI_API_KEY",
        "Codex",
        codex_login,
        refresh_token_codex,
        false,
    )
    .await?;

    // Extract account_id from stored metadata
    let account_id = load_credentials(&kv_key)?
        .and_then(|c| {
            c.metadata
                .get("account_id")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .unwrap_or_default();

    Ok((token, account_id))
}

/// Get a valid Codex API token, with interactive login if needed.
pub async fn login_codex(nickname: Option<&str>) -> Result<(String, String)> {
    let kv_key = resolve_kv_key(nickname, KV_KEY_CODEX);
    let token = get_token(
        &kv_key,
        "OPENAI_API_KEY",
        "Codex",
        codex_login,
        refresh_token_codex,
        true,
    )
    .await?;

    let account_id = load_credentials(&kv_key)?
        .and_then(|c| {
            c.metadata
                .get("account_id")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .unwrap_or_default();

    Ok((token, account_id))
}

/// Get a valid OpenRouter API key. No OAuth — uses stored key or OPENROUTER_API_KEY env var.
pub async fn get_openrouter_token(nickname: Option<&str>) -> Result<String> {
    get_openrouter_token_inner(nickname, false).await
}

pub async fn login_openrouter(nickname: Option<&str>) -> Result<String> {
    get_openrouter_token_inner(nickname, true).await
}

async fn get_openrouter_token_inner(nickname: Option<&str>, interactive: bool) -> Result<String> {
    let kv_key = resolve_kv_key(nickname, KV_KEY_OPENROUTER);

    // 1. Stored credentials (skipped on interactive login — caller wants fresh auth).
    if !interactive && let Some(creds) = load_credentials(&kv_key)? {
        return Ok(creds.access_token);
    }

    // 2. Environment variable (skipped on interactive login — would silently
    // bypass prompt + persistence under oauth:<nickname>).
    if !interactive && let Ok(key) = std::env::var("OPENROUTER_API_KEY")
        && !key.is_empty()
    {
        return Ok(key);
    }

    // 3. Interactive prompt
    eprintln!("No OpenRouter credentials found.");
    eprintln!("Get an API key from https://openrouter.ai/keys");
    eprint!("API key: ");
    let _ = std::io::stderr().flush();
    let mut key = String::new();
    std::io::stdin()
        .read_line(&mut key)
        .context("failed to read API key")?;
    let key = key.trim().to_string();
    if key.is_empty() {
        bail!("No API key provided");
    }

    // Store as OAuthCredentials for consistency
    let creds = OAuthCredentials {
        access_token: key.clone(),
        refresh_token: String::new(),
        expires_at: u64::MAX,
        metadata: serde_json::json!({}),
    };
    save_credentials(&kv_key, &creds)?;
    eprintln!("OpenRouter API key saved.");

    Ok(key)
}

/// Get a valid OpenCode API key. No OAuth — uses stored key or OPENCODE_API_KEY env var.
pub async fn get_opencode_token(nickname: Option<&str>) -> Result<String> {
    get_opencode_token_inner(nickname, false).await
}

pub async fn login_opencode(nickname: Option<&str>) -> Result<String> {
    get_opencode_token_inner(nickname, true).await
}

async fn get_opencode_token_inner(nickname: Option<&str>, interactive: bool) -> Result<String> {
    let kv_key = resolve_kv_key(nickname, KV_KEY_OPENCODE);

    if !interactive && let Some(creds) = load_credentials(&kv_key)? {
        return Ok(creds.access_token);
    }

    if !interactive && let Ok(key) = std::env::var("OPENCODE_API_KEY")
        && !key.is_empty()
    {
        return Ok(key);
    }

    // 3. Interactive prompt — open browser to auth page
    eprintln!("No OpenCode credentials found. Opening https://opencode.ai/auth ...");
    let _ = open_browser("https://opencode.ai/auth");
    eprint!("Paste API key: ");
    let _ = std::io::stderr().flush();
    let mut key = String::new();
    std::io::stdin()
        .read_line(&mut key)
        .context("failed to read API key")?;
    let key = key.trim().to_string();
    if key.is_empty() {
        bail!("No API key provided");
    }

    let creds = OAuthCredentials {
        access_token: key.clone(),
        refresh_token: String::new(),
        expires_at: u64::MAX,
        metadata: serde_json::json!({}),
    };
    save_credentials(&kv_key, &creds)?;
    eprintln!("OpenCode API key saved.");

    Ok(key)
}

/// Get a valid Grok API key. No OAuth — uses stored key or XAI_API_KEY env var.
pub async fn get_grok_token(nickname: Option<&str>) -> Result<String> {
    let kv_key = resolve_kv_key(nickname, KV_KEY_GROK);
    get_api_key_token(
        &kv_key,
        "XAI_API_KEY",
        "Grok",
        Some("https://console.x.ai/"),
        false,
    )
    .await
}

pub async fn login_grok(nickname: Option<&str>) -> Result<String> {
    let kv_key = resolve_kv_key(nickname, KV_KEY_GROK);
    get_api_key_token(
        &kv_key,
        "XAI_API_KEY",
        "Grok",
        Some("https://console.x.ai/"),
        true,
    )
    .await
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatCredentials {
    pub api_key: String,
    pub base_url: String,
    pub name: String,
}

pub async fn get_openai_compat_credentials(nickname: &str) -> Result<OpenAiCompatCredentials> {
    let kv_key = kv_key_for(nickname);
    let creds = load_credentials(&kv_key)?.with_context(|| {
        format!(
            "No OpenAI-compat credentials found for '{nickname}'.\n\
             Run: sidekar repl login oac {nickname} <base_url>"
        )
    })?;
    let base_url = creds
        .metadata
        .get("base_url")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .context("OpenAI-compat credential is missing base_url metadata")?
        .trim()
        .trim_end_matches('/')
        .to_string();
    let name = creds
        .metadata
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .unwrap_or(nickname)
        .to_string();

    Ok(OpenAiCompatCredentials {
        api_key: creds.access_token,
        base_url,
        name,
    })
}

pub async fn login_openai_compat(
    nickname: &str,
    display_name: Option<&str>,
    base_url: Option<&str>,
    api_key: Option<&str>,
) -> Result<OpenAiCompatCredentials> {
    let name = match display_name {
        Some(name) if !name.trim().is_empty() => name.trim().to_string(),
        _ => prompt_required("Provider name", Some(nickname))?,
    };
    let base_url = match base_url {
        Some(url) if !url.trim().is_empty() => url.trim().trim_end_matches('/').to_string(),
        _ => prompt_required("Base URL", None)?,
    };
    let api_key = match api_key {
        Some(key) if !key.trim().is_empty() => key.trim().to_string(),
        _ => prompt_required("API key", None)?,
    };

    save_static_token(
        &kv_key_for(nickname),
        &api_key,
        serde_json::json!({
            "provider_type": "oac",
            "name": name,
            "base_url": base_url,
        }),
    )?;
    eprintln!("OpenAI-compat API key saved for '{nickname}'.");

    Ok(OpenAiCompatCredentials {
        api_key,
        base_url,
        name,
    })
}

async fn get_api_key_token(
    kv_key: &str,
    env_var: &str,
    provider_name: &str,
    setup_url: Option<&str>,
    interactive: bool,
) -> Result<String> {
    if !interactive && let Some(creds) = load_credentials(kv_key)? {
        return Ok(creds.access_token);
    }

    if !interactive && let Ok(key) = std::env::var(env_var)
        && !key.is_empty()
    {
        return Ok(key);
    }

    if let Some(url) = setup_url {
        eprintln!("No {provider_name} credentials found. Opening {url} ...");
        let _ = open_browser(url);
    } else {
        eprintln!("No {provider_name} credentials found.");
    }
    let key = prompt_required("API key", None)?;
    save_static_token(
        kv_key,
        &key,
        serde_json::json!({
            "provider_type": provider_name.to_ascii_lowercase(),
        }),
    )?;
    eprintln!("{provider_name} API key saved.");

    Ok(key)
}

fn save_static_token(kv_key: &str, api_key: &str, metadata: serde_json::Value) -> Result<()> {
    let creds = OAuthCredentials {
        access_token: api_key.to_string(),
        refresh_token: String::new(),
        expires_at: u64::MAX,
        metadata,
    };
    save_credentials(kv_key, &creds)
}

fn prompt_required(label: &str, default: Option<&str>) -> Result<String> {
    match default {
        Some(default) => eprint!("{label} [{default}]: "),
        None => eprint!("{label}: "),
    }
    let _ = std::io::stderr().flush();
    let mut value = String::new();
    std::io::stdin()
        .read_line(&mut value)
        .with_context(|| format!("failed to read {label}"))?;
    let value = value.trim();
    let value = if value.is_empty() {
        default.unwrap_or("")
    } else {
        value
    };
    if value.is_empty() {
        bail!("No {label} provided");
    }
    Ok(value.to_string())
}

/// Generic token retrieval: stored creds → env var → error (or interactive login if `interactive`).
#[allow(clippy::type_complexity)]
async fn get_token(
    kv_key: &str,
    env_var: &str,
    provider_name: &str,
    login_fn: fn() -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<OAuthCredentials>> + Send>,
    >,
    refresh_fn: fn(
        &OAuthCredentials,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<OAuthCredentials>> + Send>,
    >,
    interactive: bool,
) -> Result<String> {
    // 1. Stored OAuth credentials.
    //    Skip during interactive login — the caller has explicitly asked to
    //    (re)authenticate this credential, so stale/existing tokens must not
    //    short-circuit the flow.
    if !interactive && let Some(creds) = load_credentials(kv_key)? {
        if creds.is_expired() {
            match refresh_fn(&creds).await {
                Ok(new_creds) => {
                    save_credentials(kv_key, &new_creds)?;
                    return Ok(new_creds.access_token);
                }
                Err(e) => {
                    eprintln!(
                        "sidekar: {provider_name} OAuth refresh failed ({e}), re-authenticating..."
                    );
                }
            }
        } else {
            return Ok(creds.access_token);
        }
    }

    // 2. Environment variable fallback — only for non-interactive usage.
    //    During `repl login`, the user expects OAuth to run and a credential
    //    row to be persisted; env-var shortcut would silently skip both.
    if !interactive && let Ok(key) = std::env::var(env_var)
        && !key.is_empty()
    {
        return Ok(key);
    }

    // 3. Interactive login (only during `repl login`) or fail
    if interactive {
        eprintln!("No {provider_name} credentials found. Starting OAuth login...");
        let creds = login_fn().await?;
        save_credentials(kv_key, &creds)?;
        Ok(creds.access_token)
    } else {
        bail!(
            "No {provider_name} credentials found for '{}'.\n\
             Run: sidekar repl login <credential>",
            kv_key.strip_prefix("oauth:").unwrap_or(kv_key)
        )
    }
}

// ---------------------------------------------------------------------------
// Anthropic OAuth
// ---------------------------------------------------------------------------

fn anthropic_login()
-> std::pin::Pin<Box<dyn std::future::Future<Output = Result<OAuthCredentials>> + Send>> {
    Box::pin(async {
        let mut creds = pkce_login(
            ANTHROPIC_CLIENT_ID,
            ANTHROPIC_AUTHORIZE_URL,
            ANTHROPIC_TOKEN_URL,
            ANTHROPIC_CALLBACK_PORT,
            "/callback",
            ANTHROPIC_SCOPES,
            "Anthropic",
            &[],
        )
        .await?;

        // Fetch profile to get account_uuid (required for API rate limit routing)
        if let Some(profile) = fetch_anthropic_profile(&creds.access_token).await {
            creds.metadata = serde_json::json!({
                "account_uuid": profile.account_uuid,
                "organization_uuid": profile.organization_uuid,
                "email": profile.email,
                "name": profile.name,
            });
        }

        Ok(creds)
    })
}

struct AnthropicProfile {
    account_uuid: String,
    organization_uuid: String,
    email: String,
    name: String,
}

async fn fetch_anthropic_profile(access_token: &str) -> Option<AnthropicProfile> {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://api.anthropic.com/api/oauth/profile")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let data: serde_json::Value = resp.json().await.ok()?;
    if crate::runtime::verbose() {
        eprintln!("[verbose] Anthropic profile response: {data}");
    }
    let account = data.get("account");
    let account_uuid = account
        .and_then(|a| a.get("uuid"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // Try common field names for email
    let email = account
        .and_then(|a| {
            a.get("email_address")
                .or_else(|| a.get("email"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("")
        .to_string();
    let name = account
        .and_then(|a| {
            a.get("full_name")
                .or_else(|| a.get("name"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("")
        .to_string();
    let organization_uuid = data
        .get("organization")
        .and_then(|o| o.get("uuid"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Some(AnthropicProfile {
        account_uuid,
        organization_uuid,
        email,
        name,
    })
}

fn refresh_token_anthropic(
    creds: &OAuthCredentials,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<OAuthCredentials>> + Send>> {
    let refresh_token = creds.refresh_token.clone();
    let metadata = creds.metadata.clone();
    Box::pin(async move {
        refresh_token_generic(
            ANTHROPIC_CLIENT_ID,
            ANTHROPIC_TOKEN_URL,
            &refresh_token,
            metadata,
        )
        .await
    })
}

// ---------------------------------------------------------------------------
// Codex OAuth
// ---------------------------------------------------------------------------

fn codex_login()
-> std::pin::Pin<Box<dyn std::future::Future<Output = Result<OAuthCredentials>> + Send>> {
    Box::pin(async {
        let extra_params = &[
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", "sidekar"),
        ];
        let mut creds = pkce_login(
            CODEX_CLIENT_ID,
            CODEX_AUTHORIZE_URL,
            CODEX_TOKEN_URL,
            CODEX_CALLBACK_PORT,
            "/auth/callback",
            CODEX_SCOPES,
            "Codex",
            extra_params,
        )
        .await?;

        // Extract account_id and email from JWT access token
        let jwt = decode_jwt_payload(&creds.access_token);
        let account_id = jwt
            .as_ref()
            .and_then(|j| {
                j.get("https://api.openai.com/auth")
                    .and_then(|auth| auth.get("chatgpt_account_id"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("")
            .to_string();
        let email = jwt
            .as_ref()
            .and_then(|j| {
                j.get("email")
                    .or_else(|| {
                        j.get("https://api.openai.com/profile")
                            .and_then(|p| p.get("email"))
                    })
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("")
            .to_string();
        creds.metadata = serde_json::json!({ "account_id": account_id, "email": email });

        Ok(creds)
    })
}

fn refresh_token_codex(
    creds: &OAuthCredentials,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<OAuthCredentials>> + Send>> {
    let refresh_token = creds.refresh_token.clone();
    let metadata = creds.metadata.clone();
    Box::pin(async move {
        let mut new_creds =
            refresh_token_generic(CODEX_CLIENT_ID, CODEX_TOKEN_URL, &refresh_token, metadata)
                .await?;
        // Re-extract account_id and email from new token
        let jwt = decode_jwt_payload(&new_creds.access_token);
        let account_id = jwt
            .as_ref()
            .and_then(|j| {
                j.get("https://api.openai.com/auth")
                    .and_then(|auth| auth.get("chatgpt_account_id"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("")
            .to_string();
        let email = jwt
            .as_ref()
            .and_then(|j| {
                j.get("email")
                    .or_else(|| {
                        j.get("https://api.openai.com/profile")
                            .and_then(|p| p.get("email"))
                    })
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("")
            .to_string();
        new_creds.metadata = serde_json::json!({ "account_id": account_id, "email": email });
        Ok(new_creds)
    })
}

/// Decode JWT payload (base64url) and return as JSON value.
fn decode_jwt_payload(token: &str) -> Option<serde_json::Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    use base64::Engine;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    if crate::runtime::verbose() {
        eprintln!("[verbose] JWT payload: {json}");
    }
    Some(json)
}

// ---------------------------------------------------------------------------
// Generic PKCE OAuth flow (shared by both providers)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn pkce_login(
    client_id: &str,
    authorize_url: &str,
    token_url: &str,
    callback_port: u16,
    callback_path: &str,
    scopes: &str,
    provider_name: &str,
    extra_params: &[(&str, &str)],
) -> Result<OAuthCredentials> {
    let verifier = generate_pkce_verifier();
    let challenge = pkce_challenge(&verifier);
    let state = generate_random_hex(32);

    let callback = format!("http://localhost:{callback_port}{callback_path}");
    let mut auth_url = format!(
        "{authorize_url}?\
        response_type=code\
        &client_id={client_id}\
        &redirect_uri={}\
        &scope={}\
        &state={state}\
        &code_challenge={challenge}\
        &code_challenge_method=S256",
        urlencoding::encode(&callback),
        urlencoding::encode(scopes),
    );
    for (k, v) in extra_params {
        auth_url.push_str(&format!("&{k}={}", urlencoding::encode(v)));
    }

    let (code_tx, code_rx) = tokio::sync::oneshot::channel::<String>();
    let server = start_callback_server(callback_port, state.clone(), code_tx).await?;

    eprintln!("\nOpening browser for {provider_name} login...");
    eprintln!("If the browser doesn't open, visit:\n{auth_url}\n");
    let _ = open_browser(&auth_url);

    let code = tokio::time::timeout(std::time::Duration::from_secs(120), code_rx)
        .await
        .context("OAuth login timed out (120s)")?
        .context("OAuth callback channel closed")?;

    server.abort();

    // Exchange code for tokens with retry on transient server errors
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "client_id": client_id,
        "code": code,
        "redirect_uri": callback,
        "code_verifier": verifier,
    });
    let mut last_err = None;
    for attempt in 0..3u32 {
        match client
            .post(token_url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    let token_resp: TokenResponse =
                        resp.json().await.context("Invalid token response")?;
                    eprintln!("Logged in to {provider_name}.");
                    return Ok(OAuthCredentials {
                        access_token: token_resp.access_token,
                        refresh_token: token_resp.refresh_token,
                        expires_at: now_secs() + token_resp.expires_in,
                        metadata: serde_json::Value::Null,
                    });
                }
                let resp_body = resp.text().await.unwrap_or_default();
                if status.is_client_error() {
                    bail!("Token exchange failed ({}): {}", status, resp_body);
                }
                last_err = Some(anyhow::anyhow!(
                    "Token exchange failed ({}): {}",
                    status,
                    resp_body
                ));
            }
            Err(e) => {
                last_err = Some(e.into());
            }
        }
        if attempt < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(500 * 2u64.pow(attempt))).await;
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Token exchange failed after retries")))
}

async fn refresh_token_generic(
    client_id: &str,
    token_url: &str,
    refresh_token: &str,
    metadata: serde_json::Value,
) -> Result<OAuthCredentials> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "client_id": client_id,
        "refresh_token": refresh_token,
    });
    let mut last_err = None;
    for attempt in 0..3u32 {
        match client
            .post(token_url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    let token_resp: TokenResponse = resp.json().await?;
                    return Ok(OAuthCredentials {
                        access_token: token_resp.access_token,
                        refresh_token: token_resp.refresh_token,
                        expires_at: now_secs() + token_resp.expires_in,
                        metadata,
                    });
                }
                let resp_body = resp.text().await.unwrap_or_default();
                if status.is_client_error() {
                    bail!("Token refresh failed ({}): {}", status, resp_body);
                }
                last_err = Some(anyhow::anyhow!(
                    "Token refresh failed ({}): {}",
                    status,
                    resp_body
                ));
            }
            Err(e) => {
                last_err = Some(e.into());
            }
        }
        if attempt < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(500 * 2u64.pow(attempt))).await;
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Token refresh failed after retries")))
}

// ---------------------------------------------------------------------------
// Token response
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

// ---------------------------------------------------------------------------
// Local callback server (shared)
// ---------------------------------------------------------------------------

async fn start_callback_server(
    port: u16,
    expected_state: String,
    code_tx: tokio::sync::oneshot::Sender<String>,
) -> Result<tokio::task::JoinHandle<()>> {
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .with_context(|| format!("Could not bind to port {port} for OAuth callback"))?;

    let handle = tokio::spawn(async move {
        let code_tx = std::sync::Mutex::new(Some(code_tx));

        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };

            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let mut buf = vec![0u8; 4096];
            let n = match stream.read(&mut buf).await {
                Ok(n) => n,
                Err(_) => continue,
            };
            let request = String::from_utf8_lossy(&buf[..n]);

            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("");

            // Accept both /callback and /auth/callback (Codex uses the latter)
            if !path.contains("callback") {
                let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
                let _ = stream.write_all(resp.as_bytes()).await;
                continue;
            }

            let query = path.split('?').nth(1).unwrap_or("");
            let params: std::collections::HashMap<&str, &str> = query
                .split('&')
                .filter_map(|p| {
                    let mut parts = p.splitn(2, '=');
                    Some((parts.next()?, parts.next()?))
                })
                .collect();

            let state = params.get("state").copied().unwrap_or("");
            let code = params.get("code").copied().unwrap_or("");

            if state != expected_state || code.is_empty() {
                let body = "Authentication failed: invalid state or missing code.";
                let resp = format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                continue;
            }

            let body = "<!DOCTYPE html><html><body>\
                <h2>Logged in!</h2>\
                <p>You can close this tab and return to the terminal.</p>\
                <script>window.close()</script>\
                </body></html>";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes()).await;

            if let Some(tx) = code_tx.lock().unwrap().take() {
                let _ = tx.send(code.to_string());
            }
            break;
        }
    });

    Ok(handle)
}

// ---------------------------------------------------------------------------
// PKCE helpers
// ---------------------------------------------------------------------------

fn generate_pkce_verifier() -> String {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
    base64_url_encode(&bytes)
}

fn pkce_challenge(verifier: &str) -> String {
    let hash = sha256_simple(verifier.as_bytes());
    base64_url_encode(&hash)
}

fn sha256_simple(input: &[u8]) -> [u8; 32] {
    use std::process::Command;
    let mut child = Command::new("openssl")
        .args(["dgst", "-sha256", "-binary"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("openssl required for OAuth PKCE");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input)
        .expect("write to openssl");
    let output = child.wait_with_output().expect("openssl sha256");
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&output.stdout[..32]);
    hash
}

fn base64_url_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn generate_random_hex(len: usize) -> String {
    let mut bytes = vec![0u8; len / 2];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// Browser launch
// ---------------------------------------------------------------------------

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// KV persistence
// ---------------------------------------------------------------------------

fn save_credentials(key: &str, creds: &OAuthCredentials) -> Result<()> {
    let json = serde_json::to_string(creds)?;
    crate::broker::kv_set(key, &json, None)
}

fn load_credentials(key: &str) -> Result<Option<OAuthCredentials>> {
    match crate::broker::kv_get(key)? {
        Some(entry) => {
            let creds: OAuthCredentials = serde_json::from_str(&entry.value)
                .context("Corrupted OAuth credentials in KV store")?;
            Ok(Some(creds))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_type_for_grok_and_compat_prefixes() {
        assert_eq!(provider_type_for("grok"), Some("grok"));
        assert_eq!(provider_type_for("grok-work"), Some("grok"));
        assert_eq!(provider_type_for("oac"), Some("oac"));
        assert_eq!(provider_type_for("oac-local"), Some("oac"));
        assert_eq!(provider_type_for("oac-lab"), Some("oac"));
        assert_eq!(provider_type_for("compat-local"), None);
        assert_eq!(provider_type_for("oai-lab"), None);
    }
}
