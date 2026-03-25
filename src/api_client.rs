use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
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

static HTTP_CLIENT_SHORT_TIMEOUT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .expect("failed to build HTTP client")
});

static HTTP_CLIENT_DOWNLOAD: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
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
    Err(anyhow::anyhow!("check_version failed after {} attempts: {:?}", MAX_RETRIES, last_err))
}

pub async fn send_telemetry(
    session_id: &str,
    version: &str,
    platform: &str,
    duration_s: u64,
    tools: &HashMap<String, u64>,
) -> Result<()> {
    let url = format!("{}/v1/telemetry", api_base());
    let payload = json!({
        "session_id": session_id,
        "version": version,
        "platform": platform,
        "duration_s": duration_s,
        "tools": tools,
    });
    let mut last_err = None;
    for attempt in 0..MAX_RETRIES {
        match HTTP_CLIENT.post(&url).json(&payload).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(());
                }
                // Don't retry on 4xx client errors
                if status.is_client_error() {
                    return Err(anyhow::anyhow!("send_telemetry failed with {}: {}", status, resp.text().await.unwrap_or_default()));
                }
                last_err = Some(anyhow::anyhow!("server error: {}", status));
            }
            Err(e) => last_err = Some(anyhow::anyhow!("{}", e)),
        }
        if attempt < MAX_RETRIES - 1 {
            tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS * (attempt as u64 + 1))).await;
        }
    }
    Err(anyhow::anyhow!("send_telemetry failed after {} attempts: {:?}", MAX_RETRIES, last_err))
}

pub async fn send_feedback(
    session_id: &str,
    version: &str,
    rating: u8,
    comment: &str,
) -> Result<()> {
    let url = format!("{}/v1/feedback", api_base());
    let payload = json!({
        "session_id": session_id,
        "version": version,
        "rating": rating,
        "comment": comment,
    });
    let mut last_err = None;
    for attempt in 0..MAX_RETRIES {
        match HTTP_CLIENT.post(&url).json(&payload).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(());
                }
                // Don't retry on 4xx client errors
                if status.is_client_error() {
                    return Err(anyhow::anyhow!("send_feedback failed with {}: {}", status, resp.text().await.unwrap_or_default()));
                }
                last_err = Some(anyhow::anyhow!("server error: {}", status));
            }
            Err(e) => last_err = Some(anyhow::anyhow!("{}", e)),
        }
        if attempt < MAX_RETRIES - 1 {
            tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS * (attempt as u64 + 1))).await;
        }
    }
    Err(anyhow::anyhow!("send_feedback failed after {} attempts: {:?}", MAX_RETRIES, last_err))
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

    let resp = HTTP_CLIENT_DOWNLOAD.get(&url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        bail!("Download failed: HTTP {status} from {url}");
    }
    let bytes = resp.bytes().await?;

    let sig_resp = HTTP_CLIENT_DOWNLOAD.get(&sig_url).send().await?;
    let sig_status = sig_resp.status();
    if !sig_status.is_success() {
        bail!("Signature download failed: HTTP {sig_status} from {sig_url}");
    }
    let sig_bytes = sig_resp.bytes().await?;

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

fn verify_signature(binary: &[u8], signature: &[u8]) -> Result<()> {
    use minisign_verify::{PublicKey, Signature};

    let sig_str = std::str::from_utf8(signature)
        .context("Signature is not valid UTF-8")?;

    let pk = PublicKey::from_base64(MINISIGN_PUBKEY)
        .context("Invalid embedded public key")?;

    let sig = Signature::decode(sig_str)
        .context("Invalid signature format")?;

    pk.verify(binary, &sig, /*allow_legacy=*/ false)
        .map_err(|e| anyhow!("Signature verification failed: {e}"))?;

    Ok(())
}
