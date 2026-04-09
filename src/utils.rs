use crate::*;

mod browser;
mod human_input;
mod keys;

pub use browser::*;
pub use human_input::*;
pub use keys::*;

pub fn parse_coordinates(args: &[String]) -> Option<(f64, f64)> {
    if args.len() == 1 {
        let arg = args[0].trim();
        if let Some((x, y)) = arg.split_once(',') {
            if let (Ok(xv), Ok(yv)) = (x.parse::<f64>(), y.parse::<f64>()) {
                return Some((xv, yv));
            }
        }
    }
    if args.len() == 2 {
        if let (Ok(x), Ok(y)) = (args[0].parse::<f64>(), args[1].parse::<f64>()) {
            return Some((x, y));
        }
    }
    None
}

/// Adjust coordinates from screenshot space to CSS space, accounting for zoom.
/// With CSS zoom, visual coordinates = CSS coordinates * zoom_factor.
/// Agent picks coords from screenshot (visual space), so divide by zoom to get CSS coords.
pub fn adjust_coords_for_zoom(ctx: &crate::AppContext, x: f64, y: f64) -> (f64, f64) {
    let zoom = ctx
        .load_session_state()
        .ok()
        .and_then(|s| s.zoom_level)
        .unwrap_or(100.0)
        / 100.0;
    if (zoom - 1.0).abs() < 0.001 {
        (x, y)
    } else {
        (x / zoom, y / zoom)
    }
}

pub fn console_arg_to_text(arg: &Value) -> String {
    arg.get("value")
        .and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            Value::Null => Some("null".to_string()),
            _ => Some(v.to_string()),
        })
        .or_else(|| {
            arg.get("description")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .unwrap_or_default()
}

pub fn map_resource_type(pattern: &str) -> Option<&'static str> {
    match pattern.to_lowercase().as_str() {
        "images" => Some("Image"),
        "css" => Some("Stylesheet"),
        "fonts" => Some("Font"),
        "media" => Some("Media"),
        "scripts" => Some("Script"),
        _ => None,
    }
}

pub fn resource_type_url_patterns(resource_type: &str) -> Vec<String> {
    match resource_type {
        "Image" => vec![
            "*.png".to_string(),
            "*.jpg".to_string(),
            "*.jpeg".to_string(),
            "*.gif".to_string(),
            "*.webp".to_string(),
            "*.svg".to_string(),
            "data:image/*".to_string(),
        ],
        "Stylesheet" => vec!["*.css".to_string()],
        "Font" => vec![
            "*.woff".to_string(),
            "*.woff2".to_string(),
            "*.ttf".to_string(),
            "*.otf".to_string(),
        ],
        "Media" => vec![
            "*.mp3".to_string(),
            "*.mp4".to_string(),
            "*.webm".to_string(),
            "*.m3u8".to_string(),
        ],
        "Script" => vec!["*.js".to_string()],
        _ => Vec::new(),
    }
}

pub fn print_frame_tree(buf: &mut String, node: &Value, depth: usize) {
    if node.is_null() {
        return;
    }
    let indent = "  ".repeat(depth);
    let frame = node.get("frame").cloned().unwrap_or(Value::Null);
    let id = frame.get("id").and_then(Value::as_str).unwrap_or("");
    let name = frame.get("name").and_then(Value::as_str).unwrap_or("");
    let url = frame.get("url").and_then(Value::as_str).unwrap_or("");
    if name.is_empty() {
        let _ = writeln!(buf, "{}[{}] {}", indent, id, url);
    } else {
        let _ = writeln!(buf, "{}[{}] name=\"{}\" {}", indent, id, name, url);
    }
    for child in node
        .get("childFrames")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        print_frame_tree(buf, &child, depth + 1);
    }
}

pub fn find_frame_in_tree(node: &Value, id_or_name: &str) -> Option<(String, String)> {
    if node.is_null() {
        return None;
    }
    let frame = node.get("frame")?;
    let id = frame.get("id").and_then(Value::as_str).unwrap_or("");
    let name = frame.get("name").and_then(Value::as_str).unwrap_or("");
    let url = frame.get("url").and_then(Value::as_str).unwrap_or("");
    if id == id_or_name || name == id_or_name {
        return Some((id.to_string(), url.to_string()));
    }
    for child in node
        .get("childFrames")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if let Some(found) = find_frame_in_tree(&child, id_or_name) {
            return Some(found);
        }
    }
    None
}

pub fn find_frame_by_url(node: &Value, target_url: &str) -> Option<(String, String)> {
    if node.is_null() {
        return None;
    }
    let frame = node.get("frame")?;
    let id = frame.get("id").and_then(Value::as_str).unwrap_or("");
    let url = frame.get("url").and_then(Value::as_str).unwrap_or("");
    if url == target_url {
        return Some((id.to_string(), url.to_string()));
    }
    for child in node
        .get("childFrames")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if let Some(found) = find_frame_by_url(&child, target_url) {
            return Some(found);
        }
    }
    None
}

/// Format Unix epoch **seconds** as UTC `YYYY-MM-DD HH:MM:SS UTC` (e.g. cookie `expires`).
pub fn epoch_to_date(epoch_seconds: i64) -> String {
    if epoch_seconds <= 0 {
        return "—".to_string();
    }
    let Ok(secs) = u64::try_from(epoch_seconds) else {
        return epoch_seconds.to_string();
    };
    let days = secs / 86_400;
    let t = secs % 86_400;
    let (y, mo, d) = unix_epoch_days_to_ymd(days);
    let hh = t / 3600;
    let mm = (t % 3600) / 60;
    let ss = t % 60;
    format!("{y:04}-{mo:02}-{d:02} {hh:02}:{mm:02}:{ss:02} UTC")
}

fn unix_epoch_days_to_ymd(mut days: u64) -> (u32, u32, u32) {
    let mut year = 1970u32;
    loop {
        let diy = if year_is_leap(year) { 366u64 } else { 365 };
        if days < diy {
            break;
        }
        days -= diy;
        year += 1;
    }
    let leap = year_is_leap(year);
    let mdays: [u32; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u32;
    for &dim in &mdays {
        if days < u64::from(dim) {
            break;
        }
        days -= u64::from(dim);
        month += 1;
    }
    let day = days as u32 + 1;
    (year, month, day)
}

fn year_is_leap(y: u32) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

pub fn human_size(size: u64) -> String {
    if size > 1_048_576 {
        format!("{:.1}MB", size as f64 / 1_048_576.0)
    } else if size > 1024 {
        format!("{}KB", (size as f64 / 1024.0).round() as u64)
    } else {
        format!("{}B", size)
    }
}

pub fn activate_browser(browser_name: &str) -> Result<()> {
    if cfg!(target_os = "macos") {
        run_osascript(&format!(
            r#"tell application "{name}" to activate
try
    tell application "{name}" to set miniaturized of window 1 to false
end try"#,
            name = browser_name
        ))?;
    } else if cfg!(target_os = "linux") {
        // Best-effort: try wmctrl, then xdotool. Both are X11 tools;
        // on Wayland without these, this is a no-op (CDP restore handles the window).
        let _ = activate_browser_linux(browser_name);
    }
    Ok(())
}

pub fn minimize_browser(browser_name: &str) -> Result<()> {
    if cfg!(target_os = "macos") {
        run_osascript(&format!(
            r#"tell application "{name}"
    repeat with w in windows
        try
            set miniaturized of w to true
        end try
    end repeat
end tell"#,
            name = browser_name
        ))?;
    } else if cfg!(target_os = "linux") {
        let _ = minimize_browser_linux(browser_name);
    }
    Ok(())
}

fn activate_browser_linux(browser_name: &str) -> Result<()> {
    // Try wmctrl first (more reliable for raising + focusing)
    if let Ok(output) = Command::new("wmctrl").args(["-a", browser_name]).output() {
        if output.status.success() {
            return Ok(());
        }
    }
    // Fallback: xdotool search by name, activate first match
    if let Ok(output) = Command::new("xdotool")
        .args(["search", "--name", browser_name])
        .output()
    {
        if let Some(wid) = String::from_utf8_lossy(&output.stdout).lines().next() {
            let _ = Command::new("xdotool")
                .args(["windowactivate", wid])
                .output();
        }
    }
    Ok(())
}

fn minimize_browser_linux(browser_name: &str) -> Result<()> {
    // Try xdotool: search by name, minimize all matches
    if let Ok(output) = Command::new("xdotool")
        .args(["search", "--name", browser_name])
        .output()
    {
        for wid in String::from_utf8_lossy(&output.stdout).lines() {
            let _ = Command::new("xdotool")
                .args(["windowminimize", wid])
                .output();
        }
    }
    Ok(())
}

fn run_osascript(script: &str) -> Result<()> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .context("osascript not found — cannot control browser windows")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Cannot control browser window: {}. \
             If using a custom CHROME_PATH, ensure the app is in /Applications.",
            stderr.trim()
        );
    }
    Ok(())
}

fn dom_query_root_and_suffix(selector: Option<&str>) -> Result<(String, String)> {
    let root = match selector {
        Some(sel) => format!("document.querySelector({})", serde_json::to_string(sel)?),
        None => "document.body".to_string(),
    };
    let selector_suffix = match selector {
        Some(sel) => format!("' for selector: ' + {}", serde_json::to_string(sel)?),
        None => "''".to_string(),
    };
    Ok((root, selector_suffix))
}

pub fn build_dom_extract_script(selector: Option<&str>) -> Result<String> {
    let (root, selector_suffix) = dom_query_root_and_suffix(selector)?;
    Ok(DOM_EXTRACT_TEMPLATE
        .replace("__SIDEKAR_ROOT__", &root)
        .replace("__SIDEKAR_SELECTOR_SUFFIX__", &selector_suffix))
}

pub fn build_read_extract_script(selector: Option<&str>) -> Result<String> {
    let (root, selector_suffix) = dom_query_root_and_suffix(selector)?;
    Ok(READ_EXTRACT_TEMPLATE
        .replace("__SIDEKAR_ROOT__", &root)
        .replace("__SIDEKAR_SELECTOR_SUFFIX__", &selector_suffix))
}

pub fn build_text_extract_script(selector: Option<&str>) -> Result<String> {
    let (root, selector_suffix) = dom_query_root_and_suffix(selector)?;
    Ok(TEXT_EXTRACT_TEMPLATE
        .replace("__SIDEKAR_ROOT__", &root)
        .replace("__SIDEKAR_SELECTOR_SUFFIX__", &selector_suffix)
        .replace("__SIDEKAR_SELECTOR_GEN__", SELECTOR_GEN_SCRIPT))
}

pub fn truncate(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let clipped = input.chars().take(max_chars).collect::<String>();
    format!("{clipped}...")
}

pub fn json_value_to_arg(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        _ => v.to_string(),
    }
}

pub fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis() as i64
}

pub fn new_session_id() -> String {
    let mut bytes = [0u8; 4];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
}

#[cfg(test)]
mod tests;
