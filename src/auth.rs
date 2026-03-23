use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

const DEFAULT_API_URL: &str = "https://sidekar.dev";
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const POLL_TIMEOUT: Duration = Duration::from_secs(15 * 60); // 15 minutes

fn api_base() -> String {
    std::env::var("SIDEKAR_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string())
}

fn auth_file() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".config")
        .join("sidekar")
        .join("auth.json")
}

/// Read the stored device token, if any.
pub fn auth_token() -> Option<String> {
    let path = auth_file();
    let content = fs::read_to_string(&path).ok()?;
    let parsed: Value = serde_json::from_str(&content).ok()?;
    parsed.get("token")?.as_str().map(|s| s.to_string())
}

/// Save a device token to ~/.config/sidekar/auth.json.
pub fn save_token(token: &str) -> Result<()> {
    let path = auth_file();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    // ISO 8601 timestamp without external crate
    let now = iso_now();
    let data = json!({
        "token": token,
        "created_at": now,
    });
    fs::write(&path, serde_json::to_string_pretty(&data)?)
        .with_context(|| format!("Failed to write {}", path.display()))?;

    // Restrict permissions to owner-only on unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

/// Run the device authorization flow: request a device code, open the browser,
/// poll until the user approves, then save the token.
pub async fn device_auth_flow() -> Result<()> {
    let base = api_base();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    // Step 1: Request device code
    let resp = client
        .post(format!("{base}/api/auth/device"))
        .json(&json!({}))
        .send()
        .await
        .context("Failed to contact sidekar.dev")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("Device auth request failed: HTTP {status} — {body}");
    }

    let data: Value = resp.json().await.context("Invalid response from /api/auth/device")?;
    let device_code = data["device_code"]
        .as_str()
        .context("Missing device_code in response")?
        .to_string();
    let user_code = data["user_code"]
        .as_str()
        .context("Missing user_code in response")?;
    let default_uri = format!("{base}/auth/device");
    let verification_uri = data["verification_uri"]
        .as_str()
        .unwrap_or(&default_uri);

    // Step 2: Show the code and open browser
    println!();
    println!("  ┌─────────────────────────────────────┐");
    println!("  │                                       │");
    println!("  │   Enter this code: {:<18} │", user_code);
    println!("  │                                       │");
    println!("  │   {:<37} │", verification_uri);
    println!("  │                                       │");
    println!("  └─────────────────────────────────────┘");
    println!();

    open_browser(verification_uri);

    // Step 3: Poll for token
    println!("Waiting for authorization...");
    let start = std::time::Instant::now();
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;

        if start.elapsed() > POLL_TIMEOUT {
            bail!("Authorization timed out after 15 minutes");
        }

        let poll_resp = client
            .post(format!("{base}/api/auth/device?action=token"))
            .json(&json!({ "device_code": device_code }))
            .send()
            .await;

        let poll_resp = match poll_resp {
            Ok(r) => r,
            Err(_) => continue, // network hiccup, retry
        };

        if !poll_resp.status().is_success() {
            continue;
        }

        let poll_data: Value = match poll_resp.json().await {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Check status
        if let Some(status) = poll_data["status"].as_str() {
            match status {
                "pending" => continue,
                "expired" => bail!("Device code expired. Run `sidekar login` again."),
                _ => {}
            }
        }

        // If we got a token, we're done
        if let Some(token) = poll_data["token"].as_str() {
            save_token(token)?;
            println!("Logged in successfully! Token saved to {}", auth_file().display());
            return Ok(());
        }
    }
}

fn open_browser(url: &str) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(cmd).arg(url).spawn();
}

/// Produce an ISO 8601 UTC timestamp string (e.g. "2026-03-23T12:00:00Z")
/// without pulling in chrono or similar.
fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Convert unix timestamp to calendar date/time
    let days = (secs / 86400) as i64;
    let time_of_day = secs % 86400;
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;

    // Days since 1970-01-01 to y/m/d (civil_from_days algorithm)
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let yr = if mo <= 2 { y + 1 } else { y };

    format!("{yr:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}
