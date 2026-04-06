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

/// Provider type for a nickname. "claude-*" → Anthropic, "codex-*" → Codex, "or-*"/"openrouter-*" → OpenRouter.
pub fn provider_type_for(nickname: &str) -> Option<&'static str> {
    if nickname.starts_with("claude") {
        Some("anthropic")
    } else if nickname.starts_with("codex") {
        Some("codex")
    } else if nickname.starts_with("or") {
        Some("openrouter")
    } else if nickname.starts_with("oc") || nickname.starts_with("opencode") {
        Some("opencode")
    } else {
        None
    }
}

/// List all stored credential nicknames.
pub fn list_credentials() -> Vec<(String, String)> {
    let entries = crate::broker::kv_list().unwrap_or_default();
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
    let kv_key = resolve_kv_key(nickname, KV_KEY_OPENROUTER);

    // 1. Stored credentials
    if let Some(creds) = load_credentials(&kv_key)? {
        return Ok(creds.access_token);
    }

    // 2. Environment variable
    if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
        if !key.is_empty() {
            return Ok(key);
        }
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
    let kv_key = resolve_kv_key(nickname, KV_KEY_OPENCODE);

    // 1. Stored credentials
    if let Some(creds) = load_credentials(&kv_key)? {
        return Ok(creds.access_token);
    }

    // 2. Environment variable
    if let Ok(key) = std::env::var("OPENCODE_API_KEY") {
        if !key.is_empty() {
            return Ok(key);
        }
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

/// Generic token retrieval: stored creds → env var → error (or interactive login if `interactive`).
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
    // 1. Stored OAuth credentials
    if let Some(creds) = load_credentials(kv_key)? {
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

    // 2. Environment variable fallback
    if let Ok(key) = std::env::var(env_var) {
        if !key.is_empty() {
            return Ok(key);
        }
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
            });
        }

        Ok(creds)
    })
}

struct AnthropicProfile {
    account_uuid: String,
    organization_uuid: String,
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
    let account_uuid = data
        .get("account")
        .and_then(|a| a.get("uuid"))
        .and_then(|v| v.as_str())
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

        // Extract account_id from JWT access token
        let account_id = extract_codex_account_id(&creds.access_token).unwrap_or_default();
        creds.metadata = serde_json::json!({ "account_id": account_id });

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
        // Re-extract account_id from new token
        let account_id = extract_codex_account_id(&new_creds.access_token).unwrap_or_default();
        new_creds.metadata = serde_json::json!({ "account_id": account_id });
        Ok(new_creds)
    })
}

/// Decode JWT payload and extract chatgpt_account_id.
fn extract_codex_account_id(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    // Decode base64url payload
    use base64::Engine;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    // Account ID is nested under "https://api.openai.com/auth"
    json.get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

// ---------------------------------------------------------------------------
// Generic PKCE OAuth flow (shared by both providers)
// ---------------------------------------------------------------------------

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
        "state": state,
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
    crate::broker::kv_set(key, &json)
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
