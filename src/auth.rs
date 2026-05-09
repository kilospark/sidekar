use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::BufRead;
use std::io::Write;
use std::time::Duration;

const DEFAULT_API_URL: &str = "https://sidekar.dev";
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const POLL_TIMEOUT: Duration = Duration::from_secs(15 * 60); // 15 minutes

static HTTP_CLIENT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("failed to build HTTP client")
});

fn api_base() -> String {
    std::env::var("SIDEKAR_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string())
}

/// Read the stored device token, if any.
pub fn auth_token() -> Option<String> {
    crate::broker::auth_get("token")
}

/// Remove the stored device token and clear in-memory encryption state.
pub fn logout() -> Result<()> {
    crate::broker::auth_clear()?;
    crate::broker::clear_encryption_key();
    crate::broker::clear_current_user_id();
    Ok(())
}

/// Save a device token.
pub fn save_token(token: &str) -> Result<()> {
    crate::broker::auth_set("token", token)?;
    crate::broker::auth_set("created_at", &iso_now())?;
    Ok(())
}

/// Run the device authorization flow: request a device code, prompt the user,
/// then (after Enter) copy the code to the clipboard, open the browser, poll until
/// the user approves, and save the token.
pub async fn device_auth_flow() -> Result<()> {
    let base = api_base();

    // Step 1: Request device code
    let resp = HTTP_CLIENT
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

    let data: Value = resp
        .json()
        .await
        .context("Invalid response from /api/auth/device")?;
    let device_code = data["device_code"]
        .as_str()
        .context("Missing device_code in response")?
        .to_string();
    let user_code = data["user_code"]
        .as_str()
        .context("Missing user_code in response")?;
    let default_uri = format!("{base}/approve");
    let verification_uri = data["verification_uri"].as_str().unwrap_or(&default_uri);

    println!();
    println!("Device code: {user_code}");
    println!("Authorization page: {verification_uri}");
    println!();

    let code = user_code.to_string();
    let url = verification_uri.to_string();
    let copied = tokio::task::spawn_blocking(move || {
        print!("Press Enter to copy the device code to the clipboard and open that page in your browser. ");
        let _ = std::io::stdout().flush();
        let mut buf = String::new();
        let _ = std::io::stdin().lock().read_line(&mut buf);
        let ok = copy_device_code_to_clipboard(&code);
        open_browser(&url);
        ok
    })
    .await
    .map_err(|e| anyhow::anyhow!("stdin task failed: {e}"))?;

    if copied {
        println!("Copied to clipboard. Opened the authorization page.");
    } else {
        println!(
            "Opened the authorization page. (Clipboard unavailable — paste the code from above.)"
        );
    }

    // Step 3: Poll for token
    println!("Waiting for authorization...");
    let start = std::time::Instant::now();
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;

        if start.elapsed() > POLL_TIMEOUT {
            bail!("Authorization timed out after 15 minutes");
        }

        let poll_resp = HTTP_CLIENT
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
                "expired" => bail!("Device code expired. Run `sidekar device login` again."),
                _ => {}
            }
        }

        // If we got a token, we're done
        if let Some(token) = poll_data["token"].as_str() {
            save_token(token)?;
            if let Err(e) = push_device_metadata(token).await {
                eprintln!("sidekar: could not register device info with sidekar.dev: {e:#}");
            }
            println!("Logged in successfully!");
            return Ok(());
        }
    }
}

fn system_hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

async fn push_device_metadata(token: &str) -> Result<()> {
    let body = json!({
        "hostname": system_hostname(),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "sidekar_version": env!("CARGO_PKG_VERSION"),
    });
    let url = format!("{}/api/auth/device?action=metadata", api_base());
    let resp = HTTP_CLIENT
        .post(url)
        .header("Authorization", format!("Bearer {}", token))
        .json(&body)
        .send()
        .await
        .context("metadata HTTP request")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        bail!("metadata failed: HTTP {status} {text}");
    }
    Ok(())
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

/// Best-effort clipboard (device code is short, alphanumeric + hyphen).
fn copy_device_code_to_clipboard(s: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        return crate::desktop::input::set_clipboard_text(s).is_ok();
    }
    #[cfg(target_os = "linux")]
    {
        use std::process::{Command, Stdio};
        if let Ok(mut child) = Command::new("wl-copy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(s.as_bytes());
            }
            if child.wait().map(|st| st.success()).unwrap_or(false) {
                return true;
            }
        }
        if let Ok(mut child) = Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(s.as_bytes());
            }
            if child.wait().map(|st| st.success()).unwrap_or(false) {
                return true;
            }
        }
        if let Ok(mut child) = Command::new("xsel")
            .args(["--clipboard", "--input"])
            .stdin(Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(s.as_bytes());
            }
            return child.wait().map(|st| st.success()).unwrap_or(false);
        }
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        // Device codes use only A-Z, digits, hyphen.
        let quoted = s.replace('\'', "''");
        return std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!("Set-Clipboard -Value '{}'", quoted),
            ])
            .status()
            .map(|st| st.success())
            .unwrap_or(false);
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = s;
        false
    }
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
