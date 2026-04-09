use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;

const DEFAULT_API_URL: &str = "https://sidekar.dev";

const MINISIGN_PUBKEY: &str = "RWRbW42KimMWVFdiSOnjFE3ZqQ3qqQ45SOySRmomdZIp3Bb9l3ZUrE33";

const TIMEOUT: Duration = Duration::from_secs(2);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const UPDATE_CHECK_INTERVAL_SECS: u64 = 60 * 60; // 1 hour

static HTTP_CLIENT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(SHUTDOWN_TIMEOUT)
        .build()
        .expect("failed to build HTTP client")
});

static HTTP_CLIENT_SHORT_TIMEOUT: std::sync::LazyLock<reqwest::Client> =
    std::sync::LazyLock::new(|| {
        reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .expect("failed to build HTTP client")
    });

static HTTP_CLIENT_DOWNLOAD: std::sync::LazyLock<reqwest::Client> =
    std::sync::LazyLock::new(|| {
        reqwest::Client::builder()
            .timeout(DOWNLOAD_TIMEOUT)
            .build()
            .expect("failed to build HTTP client")
    });

fn api_base() -> String {
    std::env::var("SIDEKAR_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string())
}

const MAX_RETRIES: u32 = 3;
const RETRY_DELAY_MS: u64 = 500;

pub async fn check_version(current: &str) -> Result<Value> {
    let url = format!("{}/v1/version?current={}", api_base(), current);
    let mut last_err = None;
    for attempt in 0..MAX_RETRIES {
        match HTTP_CLIENT_SHORT_TIMEOUT.get(&url).send().await {
            Ok(resp) => match resp.json::<Value>().await {
                Ok(v) => return Ok(v),
                Err(e) => last_err = Some(e),
            },
            Err(e) => last_err = Some(e),
        }
        if attempt < MAX_RETRIES - 1 {
            tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS * (attempt as u64 + 1))).await;
        }
    }
    Err(anyhow::anyhow!(
        "check_version failed after {} attempts: {:?}",
        MAX_RETRIES,
        last_err
    ))
}

/// Returns (platform, arch) for the current system, matching install.sh naming.
fn platform_arch() -> Result<(&'static str, &'static str)> {
    let platform = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        bail!("Unsupported OS for auto-update");
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        bail!("Unsupported architecture for auto-update");
    };
    Ok((platform, arch))
}

/// Path to the timestamp file used to throttle update checks.
fn last_check_file() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".sidekar")
        .join("last-update-check")
}

/// Returns true if enough time has passed since the last update check.
pub fn should_check_for_update() -> bool {
    let path = last_check_file();
    match std::fs::metadata(&path) {
        Ok(meta) => {
            if let Ok(modified) = meta.modified() {
                let elapsed = modified.elapsed().unwrap_or_default();
                elapsed.as_secs() >= UPDATE_CHECK_INTERVAL_SECS
            } else {
                true
            }
        }
        Err(_) => true,
    }
}

/// Touch the timestamp file to record that we just checked.
fn touch_last_check() {
    let path = last_check_file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, "");
}

/// Check for updates and return Some(latest_version) if an update is available.
/// Always records the check timestamp to prevent re-checking on every call.
pub async fn check_for_update() -> Result<Option<String>> {
    touch_last_check(); // always throttle, even if update is available
    let current = env!("CARGO_PKG_VERSION");
    let info = check_version(current).await?;
    let is_latest = info
        .get("current_is_latest")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if is_latest {
        Ok(None)
    } else {
        let latest = info
            .get("latest")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        Ok(Some(latest))
    }
}

/// Download and install the specified version, replacing the current binary.
/// Uses a lock file to prevent concurrent updates and unique temp paths to avoid collisions.
pub async fn self_update(version: &str) -> Result<()> {
    let lock_path = last_check_file().with_extension("lock");

    // Break stale locks (>5 min) before attempting atomic create
    if let Ok(meta) = std::fs::metadata(&lock_path) {
        if let Ok(modified) = meta.modified() {
            if modified.elapsed().map_or(false, |age| age.as_secs() >= 300) {
                let _ = std::fs::remove_file(&lock_path);
            }
        }
    }

    // Atomic lock acquisition — create_new fails if file already exists
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .and_then(|mut f| {
            use std::io::Write;
            write!(f, "{}", std::process::id())
        })
        .map_err(|_| anyhow!("Another update is in progress"))?;

    let result = do_self_update(version).await;

    // Always release lock
    let _ = std::fs::remove_file(&lock_path);
    result
}

async fn do_self_update(version: &str) -> Result<()> {
    let (platform, arch) = platform_arch()?;
    let asset = format!("sidekar-{platform}-{arch}");
    let url = format!("{}/download/{version}/{asset}.tar.gz", api_base());
    let sig_url = format!("{}.minisig", url);

    let bytes = {
        let mut last_err = None;
        let mut result = None;
        for attempt in 0..MAX_RETRIES {
            match HTTP_CLIENT_DOWNLOAD.get(&url).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        match resp.bytes().await {
                            Ok(b) => {
                                result = Some(b);
                                break;
                            }
                            Err(e) => last_err = Some(anyhow::anyhow!("{}", e)),
                        }
                    } else if status.is_client_error() {
                        bail!("Download failed: HTTP {status} from {url}");
                    } else {
                        last_err =
                            Some(anyhow::anyhow!("Download failed: HTTP {status} from {url}"));
                    }
                }
                Err(e) => last_err = Some(anyhow::anyhow!("{}", e)),
            }
            if attempt < MAX_RETRIES - 1 {
                tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS * (attempt as u64 + 1)))
                    .await;
            }
        }
        result
            .ok_or_else(|| last_err.unwrap_or_else(|| anyhow!("Download failed after retries")))?
    };

    let sig_bytes = {
        let mut last_err = None;
        let mut result = None;
        for attempt in 0..MAX_RETRIES {
            match HTTP_CLIENT_DOWNLOAD.get(&sig_url).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        match resp.bytes().await {
                            Ok(b) => {
                                result = Some(b);
                                break;
                            }
                            Err(e) => last_err = Some(anyhow::anyhow!("{}", e)),
                        }
                    } else if status.is_client_error() {
                        bail!("Signature download failed: HTTP {status} from {sig_url}");
                    } else {
                        last_err = Some(anyhow::anyhow!(
                            "Signature download failed: HTTP {status} from {sig_url}"
                        ));
                    }
                }
                Err(e) => last_err = Some(anyhow::anyhow!("{}", e)),
            }
            if attempt < MAX_RETRIES - 1 {
                tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS * (attempt as u64 + 1)))
                    .await;
            }
        }
        result.ok_or_else(|| {
            last_err.unwrap_or_else(|| anyhow!("Signature download failed after retries"))
        })?
    };

    verify_signature(&bytes, &sig_bytes).context("Signature verification failed")?;

    // Extract tar.gz to a unique temp dir (PID + timestamp to avoid collisions)
    let unique = format!(
        "sidekar-update-{version}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let tmp_dir = std::env::temp_dir().join(&unique);
    std::fs::create_dir_all(&tmp_dir)?;

    let gz = flate2::read::GzDecoder::new(&bytes[..]);
    let mut archive = tar::Archive::new(gz);
    archive.unpack(&tmp_dir)?;

    let extracted = tmp_dir.join(&asset);
    if !extracted.exists() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        bail!("Expected binary not found in archive: {asset}");
    }

    // Find where the current binary lives
    let current_exe =
        std::env::current_exe().context("Cannot determine current executable path")?;

    // Stage the new binary next to the target with a unique name, then atomic rename
    let staged = current_exe.with_extension(format!("new-{}", std::process::id()));
    std::fs::copy(&extracted, &staged).context("Failed to copy new binary to install directory")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755));
    }

    // Atomic swap: rename staged over current (single syscall, no gap without a binary)
    std::fs::rename(&staged, &current_exe)
        .context("Failed to replace binary (permission denied?)")?;

    let _ = std::fs::remove_dir_all(&tmp_dir);

    Ok(())
}

/// List devices registered to the current user.
pub async fn list_devices() -> Result<Value> {
    let token = crate::auth::auth_token()
        .ok_or_else(|| anyhow!("Not logged in. Run: sidekar device login"))?;

    let url = format!("{}/api/auth/devices", api_base());
    let mut last_err = None;
    for attempt in 0..MAX_RETRIES {
        match HTTP_CLIENT
            .get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return resp.json().await.context("Invalid response");
                }
                if status == reqwest::StatusCode::UNAUTHORIZED {
                    bail!("Session expired. Run: sidekar device login");
                }
                if status.is_client_error() {
                    bail!("Failed to fetch devices: HTTP {}", status);
                }
                last_err = Some(anyhow::anyhow!("Failed to fetch devices: HTTP {}", status));
            }
            Err(e) => last_err = Some(anyhow::anyhow!("{}", e)),
        }
        if attempt < MAX_RETRIES - 1 {
            tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS * (attempt as u64 + 1))).await;
        }
    }
    Err(anyhow!(
        "list_devices failed after {} attempts: {:?}",
        MAX_RETRIES,
        last_err
    ))
}

/// List active sessions for the current user.
pub async fn list_sessions() -> Result<Value> {
    let token = crate::auth::auth_token()
        .ok_or_else(|| anyhow!("Not logged in. Run: sidekar device login"))?;

    let url = format!("{}/api/sessions", api_base());
    let mut last_err = None;
    for attempt in 0..MAX_RETRIES {
        match HTTP_CLIENT
            .get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return resp.json().await.context("Invalid response");
                }
                if status == reqwest::StatusCode::UNAUTHORIZED {
                    bail!("Session expired. Run: sidekar device login");
                }
                if status.is_client_error() {
                    bail!("Failed to fetch sessions: HTTP {}", status);
                }
                last_err = Some(anyhow::anyhow!("Failed to fetch sessions: HTTP {}", status));
            }
            Err(e) => last_err = Some(anyhow::anyhow!("{}", e)),
        }
        if attempt < MAX_RETRIES - 1 {
            tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS * (attempt as u64 + 1))).await;
        }
    }
    Err(anyhow!(
        "list_sessions failed after {} attempts: {:?}",
        MAX_RETRIES,
        last_err
    ))
}

/// Register this daemon's HTTP port with the discover API.
pub async fn register_discover_port(port: u16) -> Result<()> {
    let token = crate::auth::auth_token().ok_or_else(|| anyhow!("Not logged in"))?;
    let url = format!("{}/api/sessions?discover", api_base());
    let payload = json!({"port": port});
    let mut last_err = None;
    for attempt in 0..MAX_RETRIES {
        match HTTP_CLIENT_SHORT_TIMEOUT
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(());
                }
                if status == reqwest::StatusCode::UNAUTHORIZED {
                    bail!("Session expired");
                }
                if status.is_client_error() {
                    bail!("discover register: HTTP {}", status);
                }
                last_err = Some(anyhow::anyhow!("discover register: HTTP {}", status));
            }
            Err(e) => last_err = Some(anyhow::anyhow!("{}", e)),
        }
        if attempt < MAX_RETRIES - 1 {
            tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS * (attempt as u64 + 1))).await;
        }
    }
    Err(anyhow!(
        "discover register failed after {} attempts: {:?}",
        MAX_RETRIES,
        last_err
    ))
}

/// Deregister all discover ports for this user (called on daemon shutdown).
pub async fn deregister_discover_port() {
    let Some(token) = crate::auth::auth_token() else {
        return;
    };
    let url = format!("{}/api/sessions?discover", api_base());
    let _ = HTTP_CLIENT_SHORT_TIMEOUT
        .delete(&url)
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await;
}

fn verify_signature(binary: &[u8], signature: &[u8]) -> Result<()> {
    use minisign_verify::{PublicKey, Signature};

    let sig_str = std::str::from_utf8(signature).context("Signature is not valid UTF-8")?;

    let pk = PublicKey::from_base64(MINISIGN_PUBKEY).context("Invalid embedded public key")?;

    let sig = Signature::decode(sig_str).context("Invalid signature format")?;

    pk.verify(binary, &sig, /*allow_legacy=*/ false)
        .map_err(|e| anyhow!("Signature verification failed: {e}"))?;

    Ok(())
}
