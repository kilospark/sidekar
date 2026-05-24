use super::*;

fn expand_user_path(candidate: &str) -> PathBuf {
    let trimmed = candidate.trim();
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from(env::var("HOME").unwrap_or_default()))
            .join(rest);
    }
    PathBuf::from(trimmed)
}

/// Resolve a browser executable path for existence checks and spawning. On macOS, a path may
/// traverse a `.app` that is a Finder alias (bookmark file); `std::fs` does not follow those.
/// `CHROME_PATH` may point at the bundle (`…/Foo.app`), including an alias bundle.
fn resolve_browser_executable(candidate: &str) -> Option<PathBuf> {
    let path = expand_user_path(candidate);
    if path.as_os_str().is_empty() {
        return None;
    }

    #[cfg(target_os = "macos")]
    {
        if is_macos_app_bundle(&path) && (path.is_dir() || path.is_file()) {
            return resolve_macos_bundle_root_to_executable(&path);
        }
        if path.is_file() {
            return Some(path);
        }
        return resolve_macos_alias_bundle_executable(&path).filter(|p| p.is_file());
    }
    #[cfg(not(target_os = "macos"))]
    {
        path.is_file().then_some(path)
    }
}

#[cfg(target_os = "macos")]
fn is_macos_app_bundle(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some(ext) if ext.eq_ignore_ascii_case("app")
    )
}

#[cfg(target_os = "macos")]
fn resolve_macos_bundle_root_to_executable(bundle_path: &Path) -> Option<PathBuf> {
    let resolved_bundle = if bundle_path.is_dir() {
        bundle_path.to_path_buf()
    } else if bundle_path.is_file() {
        resolve_finder_alias_posix(bundle_path)?
    } else {
        return None;
    };
    macos_main_executable_in_bundle(&resolved_bundle)
}

#[cfg(target_os = "macos")]
fn macos_main_executable_in_bundle(bundle: &Path) -> Option<PathBuf> {
    let stem = bundle.file_stem()?.to_str()?.to_string();
    let macos_dir = bundle.join("Contents/MacOS");
    let mut tries = vec![stem.clone()];
    for exe in [
        "Chromium",
        "Google Chrome",
        "Google Chrome Canary",
        "chrome",
        "Brave Browser",
        "Microsoft Edge",
        "Arc",
        "Vivaldi",
        "Opera",
    ] {
        if exe != stem.as_str() {
            tries.push(exe.into());
        }
    }
    for exe in tries {
        let p = macos_dir.join(exe);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn resolve_macos_alias_bundle_executable(exe_path: &Path) -> Option<PathBuf> {
    let path_str = exe_path.to_str()?;
    let (bundle_root_str, after_app) = path_str.split_once(".app/")?;
    let bundle_root = PathBuf::from(format!("{bundle_root_str}.app"));
    let resolved_bundle = if bundle_root.is_dir() {
        bundle_root
    } else if bundle_root.is_file() {
        resolve_finder_alias_posix(&bundle_root)?
    } else {
        return None;
    };
    let rel = after_app.trim_start_matches('/');
    Some(resolved_bundle.join(rel))
}

/// Resolve a Finder alias (bookmark) `.app` entry to the real bundle directory.
#[cfg(target_os = "macos")]
fn resolve_finder_alias_posix(bundle_alias: &Path) -> Option<PathBuf> {
    let raw = bundle_alias.to_string_lossy();
    let escaped = raw.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"tell application "Finder" to POSIX path of ((POSIX file "{}") as alias)"#,
        escaped
    );
    let output = Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let resolved = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if resolved.is_empty() {
        return None;
    }
    let pb = PathBuf::from(resolved);
    pb.is_dir().then_some(pb)
}

/// Extra Chromium bundles under `/Applications` and `~/Applications` (handles Finder aliases).
#[cfg(target_os = "macos")]
fn append_macos_chromium_bundle_candidates(candidates: &mut Vec<(String, String)>, user_apps: &Path) {
    for base in [Path::new("/Applications"), user_apps] {
        let Ok(entries) = fs::read_dir(base) else {
            continue;
        };
        for entry in entries.flatten() {
            let bundle_entry = entry.path();
            let Some(fname) = bundle_entry.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !fname.ends_with(".app") {
                continue;
            }
            let stem = fname.strip_suffix(".app").unwrap_or(fname);
            if !stem.to_lowercase().contains("chromium") {
                continue;
            }
            let bundle = if bundle_entry.is_dir() {
                bundle_entry.clone()
            } else if bundle_entry.is_file() {
                let Some(resolved) = resolve_finder_alias_posix(&bundle_entry) else {
                    continue;
                };
                resolved
            } else {
                continue;
            };
            let macos_dir = bundle.join("Contents/MacOS");
            let mut tries = vec![stem.to_string()];
            if stem != "Chromium" {
                tries.push("Chromium".into());
            }
            tries.push("chrome".into());
            for exe in tries {
                let p = macos_dir.join(&exe);
                if p.is_file() {
                    candidates.push((p.to_string_lossy().into_owned(), stem.to_string()));
                    break;
                }
            }
        }
    }
}

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
    if let Ok(chrome_path) = env::var("CHROME_PATH")
        && let Some(resolved) = resolve_browser_executable(&chrome_path)
    {
        let path = resolved.to_string_lossy().into_owned();
        let name = app_name_from_path(&path);
        return Some(BrowserCandidate { path, name });
    }

    for (path, name) in all_browser_candidates() {
        if let Some(resolved) = resolve_browser_executable(&path) {
            return Some(BrowserCandidate {
                path: resolved.to_string_lossy().into_owned(),
                name,
            });
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

/// Find a browser matching a preferred name (case-insensitive substring match on the
/// vendor label). Order: `CHROME_PATH` if it matches the preference, standard install
/// paths (`all_browser_candidates`), then PATH/`which` on non-Windows — same shells
/// `find_browser()` uses when no preference is set (Homebrew `google-chrome`, etc.).
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

    let name_matches = |label: &str| label.to_lowercase().contains(needle);

    if let Ok(chrome_path) = env::var("CHROME_PATH")
        && let Some(resolved) = resolve_browser_executable(&chrome_path)
    {
        let path = resolved.to_string_lossy().into_owned();
        let name = app_name_from_path(&path);
        if name_matches(&name) {
            return Some(BrowserCandidate { path, name });
        }
    }

    let all = all_browser_candidates();
    for (path, name) in &all {
        if !name_matches(name) {
            continue;
        }
        if let Some(resolved) = resolve_browser_executable(path) {
            return Some(BrowserCandidate {
                path: resolved.to_string_lossy().into_owned(),
                name: name.clone(),
            });
        }
    }

    if !cfg!(target_os = "windows") {
        for (bin, name) in [
            ("google-chrome-stable", "Google Chrome"),
            ("google-chrome", "Google Chrome"),
            ("chromium-browser", "Chromium"),
            ("chromium", "Chromium"),
            ("microsoft-edge-stable", "Microsoft Edge"),
            ("microsoft-edge", "Microsoft Edge"),
            ("brave-browser", "Brave Browser"),
        ] {
            if !name_matches(name) {
                continue;
            }
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

/// Return all known browser candidates for this platform (path, display name).
fn all_browser_candidates() -> Vec<(String, String)> {
    let mut candidates: Vec<(String, String)> = Vec::new();

    if cfg!(target_os = "macos") {
        let user_apps = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from(env::var("HOME").unwrap_or_default()))
            .join("Applications");
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
            candidates.push((Path::new("/Applications").join(rel).to_string_lossy().into_owned(), name.to_string()));
            candidates.push((user_apps.join(rel).to_string_lossy().into_owned(), name.to_string()));
        }
        #[cfg(target_os = "macos")]
        append_macos_chromium_bundle_candidates(&mut candidates, user_apps.as_path());
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
