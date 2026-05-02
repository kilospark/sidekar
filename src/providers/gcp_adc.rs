//! Google Cloud [Application Default Credentials](https://cloud.google.com/docs/authentication/application-default-credentials).
//!
//! Resolution order matches Google's ADC behavior:
//! 1. `GOOGLE_APPLICATION_CREDENTIALS` → JSON key file (`authorized_user` or `service_account`)
//! 2. User credentials from gcloud ADC (`application_default_credentials.json`)
//! 3. GCE/GKE metadata server (`metadata.google.internal`, then `169.254.169.254`)
//!
//! Tokens are scoped for Vertex and other GCP APIs via `cloud-platform`.

use anyhow::{Context, Result, bail};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

#[derive(Clone)]
struct Cached {
    token: String,
    refresh_after: Instant,
}

fn adc_mutex() -> &'static Mutex<Option<Cached>> {
    static CELL: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

/// Drop any cached OAuth access token so the next [`cloud_platform_access_token`] fetch is fresh.
pub async fn invalidate_cache() {
    *adc_mutex().lock().await = None;
}

/// OAuth access token (`ya29…`) suitable for `Authorization: Bearer …` against Vertex / GCP REST.
pub async fn cloud_platform_access_token() -> Result<String> {
    let mut guard = adc_mutex().lock().await;
    let now = Instant::now();
    if let Some(c) = guard.as_ref() {
        if now < c.refresh_after {
            return Ok(c.token.clone());
        }
    }

    let (token, expires_in) = fetch_token_uncached().await?;
    let ttl = Duration::from_secs(expires_in.max(120) as u64).saturating_sub(Duration::from_secs(90));
    *guard = Some(Cached {
        token: token.clone(),
        refresh_after: Instant::now() + ttl,
    });
    Ok(token)
}

async fn fetch_token_uncached() -> Result<(String, i64)> {
    let source = adc_credential_source().await?;
    match source {
        CredentialSource::AuthorizedUser(u) => refresh_user_credentials(&u).await,
        CredentialSource::ServiceAccount(sa) => exchange_service_account(&sa).await,
        CredentialSource::MetadataServer => fetch_metadata_token().await,
    }
}

#[derive(Debug)]
enum CredentialSource {
    AuthorizedUser(AuthorizedUser),
    ServiceAccount(ServiceAccount),
    MetadataServer,
}

#[derive(Debug)]
struct AuthorizedUser {
    client_id: String,
    client_secret: String,
    refresh_token: String,
}

#[derive(Debug)]
struct ServiceAccount {
    client_email: String,
    private_key: String,
    token_uri: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AdcJson {
    AuthorizedUser {
        client_id: String,
        client_secret: String,
        refresh_token: String,
    },
    ServiceAccount {
        client_email: String,
        private_key: String,
        #[serde(default)]
        token_uri: Option<String>,
    },
}

async fn adc_credential_source() -> Result<CredentialSource> {
    if let Ok(raw) = std::env::var("GOOGLE_APPLICATION_CREDENTIALS") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            return parse_adc_file(&path)
                .await
                .with_context(|| format!("GOOGLE_APPLICATION_CREDENTIALS ({})", path.display()));
        }
    }

    if let Some(path) = well_known_adc_path() {
        if path.is_file() {
            return parse_adc_file(&path)
                .await
                .with_context(|| format!("reading {}", path.display()));
        }
    }

    Ok(CredentialSource::MetadataServer)
}

fn well_known_adc_path() -> Option<PathBuf> {
    if let Ok(appdata) = std::env::var("APPDATA") {
        let p = PathBuf::from(appdata).join("gcloud/application_default_credentials.json");
        if p.is_file() {
            return Some(p);
        }
    }
    dirs::home_dir().map(|h| h.join(".config/gcloud/application_default_credentials.json"))
}

async fn parse_adc_file(path: &Path) -> Result<CredentialSource> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("failed to read {}", path.display()))?;
    let adc: AdcJson =
        serde_json::from_slice(&bytes).with_context(|| format!("invalid JSON in {}", path.display()))?;
    Ok(match adc {
        AdcJson::AuthorizedUser {
            client_id,
            client_secret,
            refresh_token,
        } => CredentialSource::AuthorizedUser(AuthorizedUser {
            client_id,
            client_secret,
            refresh_token,
        }),
        AdcJson::ServiceAccount {
            client_email,
            private_key,
            token_uri,
        } => CredentialSource::ServiceAccount(ServiceAccount {
            client_email,
            private_key,
            token_uri: token_uri.unwrap_or_else(|| "https://oauth2.googleapis.com/token".to_string()),
        }),
    })
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
}

async fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
        .context("building HTTP client for GCP auth")
}

async fn refresh_user_credentials(user: &AuthorizedUser) -> Result<(String, i64)> {
    let mut form = HashMap::new();
    form.insert("client_id", user.client_id.as_str());
    form.insert("client_secret", user.client_secret.as_str());
    form.insert("refresh_token", user.refresh_token.as_str());
    form.insert("grant_type", "refresh_token");

    let client = http_client().await?;
    let resp = client
        .post("https://oauth2.googleapis.com/token")
        .form(&form)
        .send()
        .await
        .context("POST oauth2.googleapis.com/token (refresh_token)")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!(
            "GCP user credential refresh failed ({status}): {body}\n\
             Hint: run `gcloud auth application-default login` or set GOOGLE_APPLICATION_CREDENTIALS."
        );
    }

    let tr: TokenResponse = resp.json().await.context("invalid token JSON")?;
    Ok((tr.access_token, tr.expires_in))
}

#[derive(Serialize)]
struct SaClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    exp: u64,
    iat: u64,
}

async fn exchange_service_account(sa: &ServiceAccount) -> Result<(String, i64)> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let claims = SaClaims {
        iss: &sa.client_email,
        scope: CLOUD_PLATFORM_SCOPE,
        aud: &sa.token_uri,
        iat: now,
        exp: now + 3600,
    };
    let header = Header::new(Algorithm::RS256);
    let key = EncodingKey::from_rsa_pem(sa.private_key.as_bytes())
        .context("invalid service_account private_key PEM")?;
    let jwt = encode(&header, &claims, &key).context("encode service-account JWT")?;

    let mut form = HashMap::new();
    form.insert(
        "grant_type",
        "urn:ietf:params:oauth:grant-type:jwt-bearer",
    );
    form.insert("assertion", jwt.as_str());

    let client = http_client().await?;
    let resp = client
        .post(&sa.token_uri)
        .form(&form)
        .send()
        .await
        .with_context(|| format!("POST {}", sa.token_uri))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!(
            "GCP service-account token exchange failed ({status}): {body}\n\
             Hint: verify the key file and IAM roles on `{email}`.",
            email = sa.client_email
        );
    }

    let tr: TokenResponse = resp.json().await.context("invalid token JSON")?;
    Ok((tr.access_token, tr.expires_in))
}

async fn fetch_metadata_token() -> Result<(String, i64)> {
    let client = http_client().await?;
    let urls = [
        "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token",
        "http://169.254.169.254/computeMetadata/v1/instance/service-accounts/default/token",
    ];
    let mut last_err = None;
    for url in urls {
        match client
            .get(url)
            .header("Metadata-Flavor", "Google")
            .query(&[("scopes", CLOUD_PLATFORM_SCOPE)])
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                let tr: TokenResponse = resp.json().await.context("metadata token JSON")?;
                return Ok((tr.access_token, tr.expires_in));
            }
            Ok(resp) => {
                last_err = Some(format!("HTTP {}", resp.status()));
            }
            Err(e) => last_err = Some(format!("{e:#}")),
        }
    }
    bail!(
        "GCP metadata token unavailable ({last_err})\n\
         Hint: use `gcloud auth application-default login`, set GOOGLE_APPLICATION_CREDENTIALS,\n\
         or run on GCE/GKE with a service account attached.",
        last_err = last_err.unwrap_or_else(|| "unknown".into())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn adc_json_parses_authorized_user() {
        let j = json!({
            "type": "authorized_user",
            "client_id": "cid",
            "client_secret": "sec",
            "refresh_token": "rt",
        });
        let v: AdcJson = serde_json::from_value(j).unwrap();
        match v {
            AdcJson::AuthorizedUser {
                refresh_token, ..
            } => assert_eq!(refresh_token, "rt"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn adc_json_parses_service_account() {
        let j = json!({
            "type": "service_account",
            "client_email": "x@proj.iam.gserviceaccount.com",
            "private_key": "-----BEGIN PRIVATE KEY-----\\nABC\\n-----END PRIVATE KEY-----\\n",
        });
        let v: AdcJson = serde_json::from_value(j).unwrap();
        match v {
            AdcJson::ServiceAccount { client_email, .. } => {
                assert!(client_email.contains("gserviceaccount.com"));
            }
            _ => panic!("wrong variant"),
        }
    }
}
