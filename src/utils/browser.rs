use super::*;

pub fn find_free_port() -> Result<u16> {
    let listener =
        TcpListener::bind((DEFAULT_CDP_HOST, 0)).context("failed to allocate free port")?;
    let port = listener
        .local_addr()
        .context("failed reading free port")?
        .port();
    drop(listener);
    Ok(port)
}

pub fn find_browser() -> Option<BrowserCandidate> {
    if let Ok(chrome_path) = env::var("CHROME_PATH") {
        if Path::new(&chrome_path).exists() {
            let name = app_name_from_path(&chrome_path);
            return Some(BrowserCandidate {
                path: chrome_path,
                name,
            });
        }
    }

    for (path, name) in all_browser_candidates() {
        if Path::new(&path).exists() {
            return Some(BrowserCandidate { path, name });
        }
    }

    if !cfg!(target_os = "windows") {
        for (bin, name) in [
            ("google-chrome-stable", "Google Chrome"),
            ("google-chrome", "Google Chrome"),
            ("chromium-browser", "Chromium"),
            ("chromium", "Chromium"),
            ("microsoft-edge-stable", "Microsoft Edge"),
            ("brave-browser", "Brave Browser"),
        ] {
            if let Some(path) = which_bin(bin) {
                return Some(BrowserCandidate {
                    path,
                    name: name.to_string(),
                });
            }
        }
    }

    None
}

/// Find a browser matching a preferred name (case-insensitive substring match).
/// Falls back to find_browser() if no match found.
pub fn find_browser_by_name(preferred: &str) -> Option<BrowserCandidate> {
    let pref = preferred.to_lowercase();

    // Normalize common short names to full names
    let needle = match pref.as_str() {
        "chrome" | "google-chrome" => "google chrome",
        "edge" | "msedge" => "microsoft edge",
        "brave" => "brave browser",
        "canary" | "chrome-canary" => "google chrome canary",
        other => other,
    };

    // Collect all candidates from find_browser's list and filter
    let all = all_browser_candidates();
    for (path, name) in &all {
        if name.to_lowercase().contains(needle) && Path::new(path).exists() {
            return Some(BrowserCandidate {
                path: path.clone(),
                name: name.clone(),
            });
        }
    }

    None
}

/// Return all known browser candidates for this platform (path, display name).
fn all_browser_candidates() -> Vec<(String, String)> {
    let home = env::var("HOME").unwrap_or_default();
    let mut candidates: Vec<(String, String)> = Vec::new();

    if cfg!(target_os = "macos") {
        for (name, rel) in [
            (
                "Google Chrome",
                "Google Chrome.app/Contents/MacOS/Google Chrome",
            ),
            (
                "Google Chrome Canary",
                "Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
            ),
            (
                "Microsoft Edge",
                "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            ),
            (
                "Brave Browser",
                "Brave Browser.app/Contents/MacOS/Brave Browser",
            ),
            ("Arc", "Arc.app/Contents/MacOS/Arc"),
            ("Vivaldi", "Vivaldi.app/Contents/MacOS/Vivaldi"),
            ("Opera", "Opera.app/Contents/MacOS/Opera"),
            ("Chromium", "Chromium.app/Contents/MacOS/Chromium"),
        ] {
            candidates.push((format!("/Applications/{rel}"), name.to_string()));
            candidates.push((format!("{home}/Applications/{rel}"), name.to_string()));
        }
    } else if cfg!(target_os = "linux") {
        candidates.extend(
            [
                ("/usr/bin/google-chrome-stable", "Google Chrome"),
                ("/usr/bin/google-chrome", "Google Chrome"),
                ("/usr/local/bin/google-chrome-stable", "Google Chrome"),
                ("/usr/local/bin/google-chrome", "Google Chrome"),
                ("/usr/bin/microsoft-edge-stable", "Microsoft Edge"),
                ("/usr/bin/microsoft-edge", "Microsoft Edge"),
                ("/usr/bin/brave-browser", "Brave Browser"),
                ("/usr/bin/brave-browser-stable", "Brave Browser"),
                ("/usr/bin/vivaldi-stable", "Vivaldi"),
                ("/usr/bin/vivaldi", "Vivaldi"),
                ("/usr/bin/opera", "Opera"),
                ("/usr/bin/chromium-browser", "Chromium"),
                ("/usr/bin/chromium", "Chromium"),
                ("/usr/local/bin/chromium-browser", "Chromium"),
                ("/usr/local/bin/chromium", "Chromium"),
                ("/snap/bin/chromium", "Chromium (snap)"),
            ]
            .into_iter()
            .map(|(p, n)| (p.to_string(), n.to_string())),
        );
    } else if cfg!(target_os = "windows") {
        let pf = env::var("PROGRAMFILES").unwrap_or_else(|_| "C:\\Program Files".to_string());
        let pf86 =
            env::var("PROGRAMFILES(X86)").unwrap_or_else(|_| "C:\\Program Files (x86)".to_string());
        let local = env::var("LOCALAPPDATA").unwrap_or_default();
        candidates.extend([
            (
                format!("{pf}\\Google\\Chrome\\Application\\chrome.exe"),
                "Google Chrome".to_string(),
            ),
            (
                format!("{pf86}\\Google\\Chrome\\Application\\chrome.exe"),
                "Google Chrome".to_string(),
            ),
            (
                format!("{local}\\Google\\Chrome\\Application\\chrome.exe"),
                "Google Chrome".to_string(),
            ),
            (
                format!("{pf}\\Microsoft\\Edge\\Application\\msedge.exe"),
                "Microsoft Edge".to_string(),
            ),
            (
                format!("{pf86}\\Microsoft\\Edge\\Application\\msedge.exe"),
                "Microsoft Edge".to_string(),
            ),
            (
                format!("{pf}\\BraveSoftware\\Brave-Browser\\Application\\brave.exe"),
                "Brave Browser".to_string(),
            ),
        ]);
    }

    candidates
}

/// Extract macOS app name from a path like `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`
fn app_name_from_path(path: &str) -> String {
    // Try to extract from .app bundle name (e.g., "Google Chrome.app" -> "Google Chrome")
    if let Some(idx) = path.find(".app") {
        let before_app = &path[..idx];
        if let Some(slash) = before_app.rfind('/') {
            return before_app[slash + 1..].to_string();
        }
        return before_app.to_string();
    }
    // Fall back to filename
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "chrome".to_string())
}

pub fn which_bin(bin: &str) -> Option<String> {
    let output = Command::new("which").arg(bin).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() { None } else { Some(path) }
}
