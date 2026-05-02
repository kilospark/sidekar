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
pub const KV_KEY_OPENCODE_GO: &str = "oauth:opencode-go";
pub const KV_KEY_GROK: &str = "oauth:grok";
pub const KV_KEY_GEMINI: &str = "oauth:gemini";
pub const KV_KEY_BEDROCK: &str = "oauth:bedrock";
pub const GROK_BASE_URL: &str = "https://api.x.ai";

/// Sentinel stored as OpenAI-compat `api_key` when auth uses GCP ADC ([`crate::providers::gcp_adc`]).
pub const OPENAI_COMPAT_GCP_ADC: &str = "__SIDEKAR_GCP_ADC__";

fn metadata_with_provider_type(
    provider_type: &str,
    extra_metadata: serde_json::Value,
) -> serde_json::Value {
    let mut obj = match extra_metadata {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    obj.insert(
        "provider_type".to_string(),
        serde_json::Value::String(provider_type.to_string()),
    );
    serde_json::Value::Object(obj)
}

/// KV key for a named credential. e.g., "claude-1" → "oauth:claude-1"
pub fn kv_key_for(nickname: &str) -> String {
    format!("oauth:{nickname}")
}

/// Subcommands of `/credential` — must not be used as a stored credential nickname.
const RESERVED_CREDENTIAL_NICKNAMES: &[&str] = &["list", "delete", "add", "update"];

/// Reject nicknames that collide with REPL `/credential` subcommands (ASCII case-insensitive).
pub fn validate_credential_nickname_for_storage(nickname: &str) -> Result<()> {
    let n = nickname.trim();
    if n.is_empty() {
        bail!("Credential nickname cannot be empty");
    }
    if let Some(&reserved) = RESERVED_CREDENTIAL_NICKNAMES
        .iter()
        .find(|r| r.eq_ignore_ascii_case(n))
    {
        bail!(
            "Credential nickname '{n}' is reserved — same as `/credential {reserved}`. Choose another name."
        );
    }
    Ok(())
}

fn validate_named_credential_nickname(nickname: Option<&str>) -> Result<()> {
    if let Some(n) = nickname {
        validate_credential_nickname_for_storage(n)?;
    }
    Ok(())
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
    } else if matches_convention(nickname, "ocg") || matches_convention(nickname, "opencode-go") {
        Some("opencode-go")
    } else if matches_convention(nickname, "oc") || matches_convention(nickname, "opencode") {
        Some("opencode")
    } else if matches_convention(nickname, "grok") {
        Some("grok")
    } else if matches_convention(nickname, "gem") {
        // Gemini native provider. `gem` only (not `gemini`) per user
        // convention. `gem` uniquely identifies Google Gemini among
        // sidekar's providers; `gemma` would be a different model
        // family and would require a different prefix if we ever
        // added it.
        Some("gemini")
    } else if matches_convention(nickname, "brk") || matches_convention(nickname, "bedrock") {
        Some("bedrock")
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
        Some("opencode-go") => Some("opencode-go"),
        Some("grok") => Some("grok"),
        Some("gemini") => Some("gemini"),
        Some("bedrock") => Some("bedrock"),
        Some("oac") => Some("oac"),
        Some("openai-compatible") => Some("oac"),
        _ => None,
    }
}

/// Default `oauth:<stem>` key stems (no `oauth:` prefix) → wire type.
fn legacy_kv_credential_type(stem: &str) -> Option<&'static str> {
    match stem {
        "anthropic" => Some("anthropic"),
        "codex" | "openai" => Some("codex"),
        "openrouter" => Some("openrouter"),
        "opencode" => Some("opencode"),
        "opencode-go" => Some("opencode-go"),
        "grok" => Some("grok"),
        "gemini" => Some("gemini"),
        "bedrock" | "brk" => Some("bedrock"),
        _ => None,
    }
}

/// Credential nickname or bare default-KV stem (`anthropic`, `claude-work`, …).
pub fn resolve_provider_type_for_credential(nick: &str) -> Option<&'static str> {
    provider_type_for(nick).or_else(|| legacy_kv_credential_type(nick))
}

/// `sidekar repl credential add` keyword when convention match on nickname fails.
pub fn provider_type_from_cli_keyword(keyword: &str) -> Option<&'static str> {
    match keyword {
        "claude" | "anthropic" => Some("anthropic"),
        "codex" | "openai" => Some("codex"),
        "or" | "openrouter" => Some("openrouter"),
        "oc" | "opencode" => Some("opencode"),
        "ocg" | "opencode-go" => Some("opencode-go"),
        "grok" => Some("grok"),
        "gem" | "gemini" => Some("gemini"),
        "bedrock" | "brk" => Some("bedrock"),
        _ => None,
    }
}

/// Login: prefer convention on full nickname, else CLI provider keyword.
pub fn resolve_provider_type_for_login(nickname: &str, cli_keyword: &str) -> Option<&'static str> {
    provider_type_for(nickname).or_else(|| provider_type_from_cli_keyword(cli_keyword))
}

/// Get the email/identity stored in a credential's metadata.
pub fn credential_email(nickname: &str) -> Option<String> {
    let key = kv_key_for(nickname);
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
            let provider = resolve_provider_type_for_credential(name).unwrap_or("unknown");
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

type PinOAuthFut =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<OAuthCredentials>> + Send>>;
type OAuthRefreshFn = fn(&OAuthCredentials) -> PinOAuthFut;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Force-refresh the stored access token for `cred_name` using its refresh_token,
/// regardless of the cached `expires_at`. Called after a 401 from the provider —
/// the server rejected a token we thought was still valid (clock drift,
/// early rotation, or revocation). Supports Anthropic/Codex OAuth and
/// OpenAI-compat credentials configured with GCP ADC (`metadata.auth = gcp_adc`).
pub async fn force_refresh_token(cred_name: &str) -> Result<String> {
    let provider_type = resolve_provider_type_for_credential(cred_name)
        .ok_or_else(|| anyhow::anyhow!("unknown credential '{cred_name}'"))?;

    if provider_type == "oac" {
        let kv_key = kv_key_for(cred_name);
        let creds = load_credentials(&kv_key)?.with_context(|| {
            format!(
                "no stored credentials for OpenAI-compat credential '{cred_name}'"
            )
        })?;
        if creds.metadata.get("auth").and_then(|v| v.as_str()) != Some("gcp_adc") {
            anyhow::bail!(
                "credential '{cred_name}' is not using GCP ADC — update the API key or add a new oac credential"
            );
        }
        crate::providers::gcp_adc::invalidate_cache().await;
        return crate::providers::gcp_adc::cloud_platform_access_token().await;
    }

    let (kv_key, refresh_fn): (String, OAuthRefreshFn) = match provider_type {
        "anthropic" => (
            resolve_kv_key(Some(cred_name), KV_KEY_ANTHROPIC),
            refresh_token_anthropic,
        ),
        "codex" => (
            resolve_kv_key(Some(cred_name), KV_KEY_CODEX),
            refresh_token_codex,
        ),
        other => anyhow::bail!(
            "provider '{other}' has no refresh flow — re-authenticate via `sidekar repl credential add <provider> [name]`"
        ),
    };

    let creds = load_credentials(&kv_key)?
        .ok_or_else(|| anyhow::anyhow!("no stored credentials for '{cred_name}'"))?;
    if creds.refresh_token.is_empty() {
        anyhow::bail!(
            "credential '{cred_name}' has no refresh token — re-authenticate via `sidekar repl credential add <provider> [name]`"
        );
    }
    let new_creds = refresh_fn(&creds).await?;
    save_credentials(&kv_key, &new_creds)?;
    Ok(new_creds.access_token)
}

/// Get a valid Anthropic API token. If `nickname` is provided, use that credential set.
pub async fn get_anthropic_token(nickname: Option<&str>) -> Result<String> {
    let kv_key = resolve_kv_key(nickname, KV_KEY_ANTHROPIC);
    get_token(&kv_key, "ANTHROPIC_API_KEY", "Anthropic", refresh_token_anthropic).await
}

fn codex_account_id_from_kv(kv_key: &str) -> Result<String> {
    Ok(load_credentials(kv_key)?
        .and_then(|c| {
            c.metadata
                .get("account_id")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .unwrap_or_default())
}

/// Get a valid Codex API token. If `nickname` is provided, use that credential set.
pub async fn get_codex_token(nickname: Option<&str>) -> Result<(String, String)> {
    let kv_key = resolve_kv_key(nickname, KV_KEY_CODEX);
    let token = get_token(&kv_key, "OPENAI_API_KEY", "Codex", refresh_token_codex).await?;
    Ok((token, codex_account_id_from_kv(&kv_key)?))
}

/// Get a valid OpenRouter API key. No OAuth — uses stored key or OPENROUTER_API_KEY env var.
pub async fn get_openrouter_token(nickname: Option<&str>) -> Result<String> {
    let kv_key = resolve_kv_key(nickname, KV_KEY_OPENROUTER);
    get_api_key_token(&kv_key, &["OPENROUTER_API_KEY"], "OpenRouter").await
}

/// Get a valid OpenCode API key. No OAuth — uses stored key or OPENCODE_API_KEY env var.
pub async fn get_opencode_token(nickname: Option<&str>) -> Result<String> {
    let kv_key = resolve_kv_key(nickname, KV_KEY_OPENCODE);
    get_api_key_token(&kv_key, &["OPENCODE_API_KEY"], "OpenCode").await
}

/// Get a valid OpenCode Go API key. Same key as OpenCode Zen, separate KV slot.
pub async fn get_opencode_go_token(nickname: Option<&str>) -> Result<String> {
    let kv_key = resolve_kv_key(nickname, KV_KEY_OPENCODE_GO);
    get_api_key_token(&kv_key, &["OPENCODE_API_KEY"], "OpenCode Go").await
}

/// Get a valid Grok API key. No OAuth — uses stored key or XAI_API_KEY env var.
pub async fn get_grok_token(nickname: Option<&str>) -> Result<String> {
    let kv_key = resolve_kv_key(nickname, KV_KEY_GROK);
    get_api_key_token(&kv_key, &["XAI_API_KEY"], "Grok").await
}

/// Get a valid Gemini API key. No OAuth — uses stored key or
/// GEMINI_API_KEY / GOOGLE_API_KEY env var. Gemini static keys don't
/// expire, so `force_refresh_token` bails with a re-login hint rather
/// than silently succeeding.
pub async fn get_gemini_token(nickname: Option<&str>) -> Result<String> {
    let kv_key = resolve_kv_key(nickname, KV_KEY_GEMINI);
    if std::env::var("GEMINI_API_KEY").is_err()
        && let Ok(key) = std::env::var("GOOGLE_API_KEY")
        && !key.trim().is_empty()
    {
        return Ok(key);
    }
    get_api_key_token(&kv_key, &["GEMINI_API_KEY"], "Gemini").await
}

#[derive(Debug, Clone)]
pub struct BedrockStored {
    pub region: String,
    pub aws_profile: Option<String>,
}

pub fn load_bedrock_stored(nickname: &str) -> Result<BedrockStored> {
    let kv_key = kv_key_for(nickname);
    let creds = load_credentials(&kv_key)?.with_context(|| {
        format!(
            "No Bedrock credential for '{nickname}'.\n\
             Run: sidekar repl credential add bedrock [nickname]"
        )
    })?;
    if creds.metadata.get("provider_type").and_then(|v| v.as_str()) != Some("bedrock") {
        bail!("credential '{nickname}' is not Bedrock");
    }
    let region = creds
        .metadata
        .get("region")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .context("Bedrock credential missing metadata.region")?
        .to_string();
    let aws_profile = creds
        .metadata
        .get("profile")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    Ok(BedrockStored {
        region,
        aws_profile,
    })
}

pub fn save_bedrock_credential(
    nickname: &str,
    region: &str,
    aws_profile: Option<&str>,
) -> Result<()> {
    validate_credential_nickname_for_storage(nickname)?;
    let kv_key = kv_key_for(nickname);
    let profile_val = aws_profile
        .filter(|s| !s.trim().is_empty())
        .map(|s| serde_json::json!(s.trim()))
        .unwrap_or(serde_json::Value::Null);

    save_static_token(
        &kv_key,
        "_",
        serde_json::json!({
            "provider_type": "bedrock",
            "region": region.trim(),
            "profile": profile_val,
        }),
    )
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
             Run: sidekar repl credential add oac {nickname} <base_url>"
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

    let api_key = if creds.metadata.get("auth").and_then(|v| v.as_str()) == Some("gcp_adc") {
        OPENAI_COMPAT_GCP_ADC.to_string()
    } else {
        creds.access_token.clone()
    };

    Ok(OpenAiCompatCredentials {
        api_key,
        base_url,
        name,
    })
}

/// Resolve a stored OpenAI-compat secret for HTTP (`Bearer`). GCP ADC placeholders become fresh OAuth access tokens.
pub async fn resolve_openai_compat_api_key(api_key: &str) -> Result<String> {
    if api_key == OPENAI_COMPAT_GCP_ADC {
        crate::providers::gcp_adc::cloud_platform_access_token().await
    } else {
        Ok(api_key.to_string())
    }
}

pub async fn login_openai_compat(
    nickname: &str,
    display_name: Option<&str>,
    base_url: Option<&str>,
    api_key: Option<&str>,
) -> Result<OpenAiCompatCredentials> {
    let name = display_name
        .filter(|name| !name.trim().is_empty())
        .map(|name| name.trim().to_string())
        .context("Provider name is required")?;
    let base_url = base_url
        .filter(|url| !url.trim().is_empty())
        .map(|url| url.trim().trim_end_matches('/').to_string())
        .context("Base URL is required")?;
    let api_key_trimmed = api_key
        .filter(|key| !key.trim().is_empty())
        .map(|key| key.trim().to_string())
        .context("API key is required (use `adc` for GCP Application Default Credentials)")?;

    if api_key_trimmed.eq_ignore_ascii_case("adc")
        || api_key_trimmed.eq_ignore_ascii_case("gcp-adc")
    {
        save_openai_compat_adc(nickname, &name, &base_url)
    } else {
        save_openai_compat_credential(nickname, &name, &base_url, &api_key_trimmed)
    }
}

pub fn save_openai_compat_credential(
    nickname: &str,
    display_name: &str,
    base_url: &str,
    api_key: &str,
) -> Result<OpenAiCompatCredentials> {
    validate_credential_nickname_for_storage(nickname)?;
    let name = display_name.trim().to_string();
    let base_url = base_url.trim().trim_end_matches('/').to_string();
    let api_key = api_key.trim().to_string();

    save_static_token(
        &kv_key_for(nickname),
        &api_key,
        serde_json::json!({
            "provider_type": "oac",
            "name": name,
            "base_url": base_url,
        }),
    )?;

    Ok(OpenAiCompatCredentials {
        api_key,
        base_url,
        name,
    })
}

pub fn save_openai_compat_adc(
    nickname: &str,
    display_name: &str,
    base_url: &str,
) -> Result<OpenAiCompatCredentials> {
    validate_credential_nickname_for_storage(nickname)?;
    let name = display_name.trim().to_string();
    let base_url = base_url.trim().trim_end_matches('/').to_string();

    save_static_token(
        &kv_key_for(nickname),
        "",
        serde_json::json!({
            "provider_type": "oac",
            "auth": "gcp_adc",
            "name": name,
            "base_url": base_url,
        }),
    )?;

    Ok(OpenAiCompatCredentials {
        api_key: OPENAI_COMPAT_GCP_ADC.to_string(),
        base_url,
        name,
    })
}

pub fn save_api_key_credential(
    kv_key: &str,
    provider_type: &str,
    api_key: &str,
    extra_metadata: serde_json::Value,
) -> Result<()> {
    save_static_token(
        kv_key,
        api_key.trim(),
        metadata_with_provider_type(provider_type, extra_metadata),
    )
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

/// Generic token retrieval: stored creds → env var → error.
#[allow(clippy::type_complexity)]
async fn get_api_key_token(
    kv_key: &str,
    env_vars: &[&str],
    provider_name: &str,
) -> Result<String> {
    if let Some(creds) = load_credentials(kv_key)? {
        return Ok(creds.access_token);
    }
    for name in env_vars {
        if let Ok(key) = std::env::var(name)
            && !key.is_empty()
        {
            return Ok(key);
        }
    }
    bail!(
        "No {provider_name} credentials found for '{}'.\n\
         Run: sidekar repl credential add <provider> [name]",
        kv_key.strip_prefix("oauth:").unwrap_or(kv_key)
    )
}

async fn get_token(
    kv_key: &str,
    env_var: &str,
    provider_name: &str,
    refresh_fn: fn(
        &OAuthCredentials,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<OAuthCredentials>> + Send>,
    >,
) -> Result<String> {
    if let Some(creds) = load_credentials(kv_key)? {
        if creds.is_expired() {
            match refresh_fn(&creds).await {
                Ok(new_creds) => {
                    save_credentials(kv_key, &new_creds)?;
                    return Ok(new_creds.access_token);
                }
                Err(e) => {
                    crate::broker::try_log_error(
                        "oauth",
                        &format!("{provider_name} OAuth refresh failed"),
                        Some(&format!("{e:#}")),
                    );
                    crate::broker::try_log_event(
                        "debug",
                        "oauth",
                        "refresh-failed-reauthenticating",
                        Some(provider_name),
                    );
                }
            }
        } else {
            return Ok(creds.access_token);
        }
    }

    if let Ok(key) = std::env::var(env_var)
        && !key.is_empty()
    {
        return Ok(key);
    }

    bail!(
        "No {provider_name} credentials found for '{}'.\n\
         Run: sidekar repl credential add <provider> [name]",
        kv_key.strip_prefix("oauth:").unwrap_or(kv_key)
    )
}

// ---------------------------------------------------------------------------
// Anthropic OAuth
// ---------------------------------------------------------------------------

struct AnthropicProfile {
    account_uuid: String,
    organization_uuid: String,
    email: String,
    name: String,
}

async fn fetch_anthropic_profile(access_token: &str) -> Option<AnthropicProfile> {
    let client = super::catalog_http_client(super::MODEL_CATALOG_TIMEOUT_SECS).ok()?;
    let resp = client
        .get("https://api.anthropic.com/api/oauth/profile")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let data: serde_json::Value = resp.json().await.ok()?;
    if crate::runtime::verbose() {
        crate::broker::try_log_event(
            "debug",
            "oauth",
            "anthropic-profile-response",
            Some(&data.to_string()),
        );
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

fn codex_credentials_metadata(access_token: &str) -> serde_json::Value {
    let jwt = decode_jwt_payload(access_token);
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
    serde_json::json!({ "account_id": account_id, "email": email })
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
        new_creds.metadata = codex_credentials_metadata(&new_creds.access_token);
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
        crate::broker::try_log_event("debug", "oauth", "jwt-payload", Some(&json.to_string()));
    }
    Some(json)
}

// ---------------------------------------------------------------------------
// Generic PKCE OAuth flow (shared by both providers)
// ---------------------------------------------------------------------------

pub(crate) struct InteractiveOAuthLogin {
    pub provider_name: &'static str,
    pub auth_url: String,
    kv_key: String,
    pending: PendingPkceLogin,
}

struct PendingPkceLogin {
    client_id: &'static str,
    token_url: &'static str,
    callback: String,
    verifier: String,
    state: String,
    include_state_in_token_exchange: bool,
    code_rx: tokio::sync::oneshot::Receiver<String>,
    server: tokio::task::JoinHandle<()>,
}

pub(crate) async fn begin_anthropic_login(nickname: Option<&str>) -> Result<InteractiveOAuthLogin> {
    validate_named_credential_nickname(nickname)?;
    let kv_key = resolve_kv_key(nickname, KV_KEY_ANTHROPIC);
    let (auth_url, pending) = begin_pkce_login(
        ANTHROPIC_CLIENT_ID,
        ANTHROPIC_AUTHORIZE_URL,
        ANTHROPIC_TOKEN_URL,
        ANTHROPIC_CALLBACK_PORT,
        "/callback",
        ANTHROPIC_SCOPES,
        &[],
        true,
    )
    .await?;
    Ok(InteractiveOAuthLogin {
        provider_name: "Anthropic",
        auth_url,
        kv_key,
        pending,
    })
}

pub(crate) async fn finish_anthropic_login(login: InteractiveOAuthLogin) -> Result<String> {
    let (kv_key, mut creds) = complete_interactive_login(login).await?;
    if let Some(profile) = fetch_anthropic_profile(&creds.access_token).await {
        creds.metadata = serde_json::json!({
            "account_uuid": profile.account_uuid,
            "organization_uuid": profile.organization_uuid,
            "email": profile.email,
            "name": profile.name,
        });
    }
    save_credentials(&kv_key, &creds)?;
    Ok(creds.access_token)
}

pub(crate) async fn begin_codex_login(nickname: Option<&str>) -> Result<InteractiveOAuthLogin> {
    validate_named_credential_nickname(nickname)?;
    let kv_key = resolve_kv_key(nickname, KV_KEY_CODEX);
    let extra_params = &[
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "sidekar"),
    ];
    let (auth_url, pending) = begin_pkce_login(
        CODEX_CLIENT_ID,
        CODEX_AUTHORIZE_URL,
        CODEX_TOKEN_URL,
        CODEX_CALLBACK_PORT,
        "/auth/callback",
        CODEX_SCOPES,
        extra_params,
        false,
    )
    .await?;
    Ok(InteractiveOAuthLogin {
        provider_name: "Codex",
        auth_url,
        kv_key,
        pending,
    })
}

pub(crate) async fn finish_codex_login(login: InteractiveOAuthLogin) -> Result<(String, String)> {
    let (kv_key, mut creds) = complete_interactive_login(login).await?;
    creds.metadata = codex_credentials_metadata(&creds.access_token);
    save_credentials(&kv_key, &creds)?;
    Ok((creds.access_token.clone(), codex_account_id_from_kv(&kv_key)?))
}

async fn complete_interactive_login(
    login: InteractiveOAuthLogin,
) -> Result<(String, OAuthCredentials)> {
    let creds = complete_pkce_login(login.pending).await?;
    Ok((login.kv_key, creds))
}

#[allow(clippy::too_many_arguments)]
async fn begin_pkce_login(
    client_id: &'static str,
    authorize_url: &'static str,
    token_url: &'static str,
    callback_port: u16,
    callback_path: &'static str,
    scopes: &'static str,
    extra_params: &[(&str, &str)],
    include_state_in_token_exchange: bool,
) -> Result<(String, PendingPkceLogin)> {
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

    Ok((
        auth_url,
        PendingPkceLogin {
            client_id,
            token_url,
            callback,
            verifier,
            state,
            include_state_in_token_exchange,
            code_rx,
            server,
        },
    ))
}

async fn complete_pkce_login(pending: PendingPkceLogin) -> Result<OAuthCredentials> {
    let PendingPkceLogin {
        client_id,
        token_url,
        callback,
        verifier,
        state,
        include_state_in_token_exchange,
        code_rx,
        server,
    } = pending;
    let code = tokio::time::timeout(std::time::Duration::from_secs(120), code_rx)
        .await
        .context("OAuth login timed out (120s)")?
        .context("OAuth callback channel closed")?;

    server.abort();

    // Exchange code for tokens with retry on transient server errors.
    // `state` inclusion is provider-dependent — see docs on
    // `include_state_in_token_exchange`. Delegated to a pure helper
    // so the body shape has a unit test that locks in both variants;
    // the last two regressions on this code path were caused by
    // blind edits to an inline json!{} literal.
    let client = reqwest::Client::new();
    let body = build_token_exchange_body(
        client_id,
        &code,
        &callback,
        &verifier,
        &state,
        include_state_in_token_exchange,
    );
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

pub fn open_browser_url(url: &str) -> Result<()> {
    open_browser(url)
}

// ---------------------------------------------------------------------------
// KV persistence
// ---------------------------------------------------------------------------

pub(crate) fn save_credentials(key: &str, creds: &OAuthCredentials) -> Result<()> {
    let json = serde_json::to_string(creds)?;
    crate::broker::kv_set(key, &json, None)
}

pub(crate) fn load_credentials(key: &str) -> Result<Option<OAuthCredentials>> {
    match crate::broker::kv_get(key)? {
        Some(entry) => {
            let creds: OAuthCredentials = serde_json::from_str(&entry.value)
                .context("Corrupted OAuth credentials in KV store")?;
            Ok(Some(creds))
        }
        None => Ok(None),
    }
}

/// Build the body of an OAuth token-exchange POST. Pure; no network
/// IO. The `include_state` flag exists only to accommodate the
/// divergent behaviors of Anthropic (requires `state`) and OpenAI
/// (rejects `state`). See `pkce_login` for the full rationale.
fn build_token_exchange_body(
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
    state: &str,
    include_state: bool,
) -> serde_json::Value {
    if include_state {
        serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": client_id,
            "code": code,
            "state": state,
            "redirect_uri": redirect_uri,
            "code_verifier": code_verifier,
        })
    } else {
        serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": client_id,
            "code": code,
            "redirect_uri": redirect_uri,
            "code_verifier": code_verifier,
        })
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

    #[test]
    fn provider_type_for_gemini_uses_gem_prefix() {
        // Gemini nicknames start with `gem`. The `gem-` variant is
        // the canonical multi-credential form (gem-work, gem-test).
        assert_eq!(provider_type_for("gem"), Some("gemini"));
        assert_eq!(provider_type_for("gem-work"), Some("gemini"));
        assert_eq!(provider_type_for("gem-test"), Some("gemini"));
    }

    #[test]
    fn resolve_credential_type_handles_default_kv_stems() {
        assert_eq!(
            resolve_provider_type_for_credential("anthropic"),
            Some("anthropic")
        );
        assert_eq!(
            resolve_provider_type_for_credential("openai"),
            Some("codex")
        );
        assert_eq!(
            resolve_provider_type_for_credential("gemini"),
            Some("gemini")
        );
    }

    #[test]
    fn credential_nickname_reserved_for_repl_slash_commands() {
        for nick in ["list", "LIST", "delete", "add", "update"] {
            assert!(
                validate_credential_nickname_for_storage(nick).is_err(),
                "expected reserved: {nick}"
            );
        }
        assert!(validate_credential_nickname_for_storage("login").is_ok());
        assert!(validate_credential_nickname_for_storage("claude-work").is_ok());
        assert!(validate_credential_nickname_for_storage("adder").is_ok());
    }

    #[test]
    fn resolve_login_falls_back_to_cli_keyword() {
        assert_eq!(
            resolve_provider_type_for_login("weird-nick", "claude"),
            Some("anthropic")
        );
    }

    #[test]
    fn metadata_helper_injects_provider_type() {
        let meta = metadata_with_provider_type("openrouter", serde_json::json!({}));
        assert_eq!(meta["provider_type"], "openrouter");

        let meta = metadata_with_provider_type(
            "gemini",
            serde_json::json!({ "base_url": "https://example.test/v1" }),
        );
        assert_eq!(meta["provider_type"], "gemini");
        assert_eq!(meta["base_url"], "https://example.test/v1");
    }

    // ─── token exchange body shape ─────────────────────────────
    //
    // These tests lock in the body contract for both provider
    // variants. They have saved us from exactly this regression
    // three times now (v1.0.40, v2.5.29, and this commit). Do NOT
    // delete them; if a future provider requires a new shape, add
    // a new flag and a new test — don't generalize by deleting
    // pins.

    fn parse(v: &serde_json::Value) -> std::collections::BTreeMap<&str, &serde_json::Value> {
        v.as_object()
            .expect("body is JSON object")
            .iter()
            .map(|(k, v)| (k.as_str(), v))
            .collect()
    }

    #[test]
    fn token_exchange_body_includes_state_for_anthropic_variant() {
        // Anthropic contract: `state` MUST be present. Omitting it
        // causes platform.claude.com to return 400 "Invalid request
        // format". All six fields present, nothing else.
        let body = build_token_exchange_body(
            "CLIENT",
            "CODE",
            "http://localhost:54321/callback",
            "VERIFIER",
            "STATE123",
            true,
        );
        let map = parse(&body);
        assert_eq!(map.len(), 6, "exactly six fields");
        assert_eq!(map["grant_type"], "authorization_code");
        assert_eq!(map["client_id"], "CLIENT");
        assert_eq!(map["code"], "CODE");
        assert_eq!(map["state"], "STATE123");
        assert_eq!(map["redirect_uri"], "http://localhost:54321/callback");
        assert_eq!(map["code_verifier"], "VERIFIER");
    }

    #[test]
    fn token_exchange_body_omits_state_for_openai_variant() {
        // OpenAI contract: `state` MUST NOT be present. Including
        // it causes auth.openai.com to return 400 "Unknown
        // parameter: 'state'". Five fields, state absent, nothing
        // else.
        let body = build_token_exchange_body(
            "CLIENT",
            "CODE",
            "http://localhost:1455/auth/callback",
            "VERIFIER",
            "STATE123",
            false,
        );
        let map = parse(&body);
        assert_eq!(map.len(), 5, "exactly five fields");
        assert!(
            !map.contains_key("state"),
            "OpenAI variant must NOT include state; regression test"
        );
        assert_eq!(map["grant_type"], "authorization_code");
        assert_eq!(map["client_id"], "CLIENT");
        assert_eq!(map["code"], "CODE");
        assert_eq!(map["redirect_uri"], "http://localhost:1455/auth/callback");
        assert_eq!(map["code_verifier"], "VERIFIER");
    }

    #[test]
    fn token_exchange_body_openai_variant_is_rfc6749_compliant() {
        // Meta-assertion: the five OpenAI fields are exactly the
        // ones RFC 6749 §4.1.3 defines. If a future provider needs
        // a different RFC-compliant shape (e.g. PKCE without PKCE
        // — unusual but valid), add a new flag rather than
        // loosening this assertion.
        let body = build_token_exchange_body("C", "K", "R", "V", "S", false);
        let map = parse(&body);
        let keys: std::collections::BTreeSet<&str> = map.keys().copied().collect();
        let expected: std::collections::BTreeSet<&str> = [
            "grant_type",
            "client_id",
            "code",
            "redirect_uri",
            "code_verifier",
        ]
        .iter()
        .copied()
        .collect();
        assert_eq!(keys, expected);
    }

    #[test]
    fn provider_type_for_gemini_does_not_collide_with_other_gem_prefixes() {
        // Hypothetical future models whose names start with `gem`
        // must NOT be claimed by the Gemini provider. matches_convention
        // requires exact prefix match or `prefix-` form, so `gemma`
        // and `gemstones` naturally miss, but pin them in a test so a
        // future change to the matcher doesn't silently break this.
        assert_eq!(provider_type_for("gemma"), None);
        assert_eq!(provider_type_for("gemstones"), None);
        assert_eq!(provider_type_for("gemini-model"), None);
        // Exact `gemini` (no dash) is also not accepted as a prefix —
        // callers who want to use the bare name should use `gem`.
        // If we ever add `gemini` as an accepted prefix, update here.
        assert_eq!(provider_type_for("gemini"), None);
    }
}
