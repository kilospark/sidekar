//! Load screenshot files referenced in tool stdout into [`ToolResultInlineImage`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use base64::Engine;

use crate::providers::ToolResultInlineImage;

/// Recognizes desktop/page capture (`desktop screenshot`, `screenshot`) and
/// Chrome extension capture (`ext screenshot`).
fn screenshot_line_paths(line: &str) -> Option<&str> {
    const TO: &str = "Screenshot saved to ";
    const EXT: &str = "Screenshot saved: ";
    let line = line.trim();
    line.strip_prefix(TO)
        .or_else(|| line.strip_prefix(EXT))
        .map(str::trim)
}

/// Per-image cap — keeps tool-result payloads bounded (~11MB base64 worst case below multimodal limits).
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

/// Strip terminal ANSI (tool output may include color codes) then collect unique paths.
pub(crate) fn augment_tool_output_with_screenshot_images(
    output: String,
) -> (String, Vec<ToolResultInlineImage>) {
    let cleaned = crate::runtime::strip_ansi(&output);
    let paths = collect_screenshot_paths(&cleaned);
    let mut images = Vec::new();
    for p in paths {
        if let Ok(img) = load_tool_result_image(&p) {
            images.push(img);
        }
    }
    (output, images)
}

fn collect_screenshot_paths(stripped_output: &str) -> Vec<PathBuf> {
    let mut seen = HashSet::<String>::new();
    let mut out = Vec::new();
    for line in stripped_output.lines() {
        let Some(path) = screenshot_line_paths(line) else {
            continue;
        };
        if path.is_empty() || !seen.insert(path.to_string()) {
            continue;
        }
        out.push(PathBuf::from(path));
    }
    out
}

fn guess_media_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

fn load_tool_result_image(path: &Path) -> anyhow::Result<ToolResultInlineImage> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > MAX_IMAGE_BYTES {
        anyhow::bail!(
            "screenshot too large for inline tool attach (max {} MiB): {}",
            MAX_IMAGE_BYTES / 1024 / 1024,
            path.display()
        );
    }
    let bytes = std::fs::read(path)?;
    let media_type = guess_media_type(path).to_string();
    if !matches!(
        media_type.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        anyhow::bail!("not a recognized image type: {}", path.display());
    }
    Ok(ToolResultInlineImage {
        media_type,
        data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn collects_path_from_sidekar_screenshot_line() {
        let png = std::env::temp_dir().join(format!(
            "sidekar-tool-attach-test-{}.png",
            rand::random::<u64>()
        ));
        // Minimal 1x1 PNG
        let png_bytes: [u8; 67] = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let mut f = std::fs::File::create(&png).unwrap();
        f.write_all(&png_bytes).unwrap();

        let stdout = format!(
            "Something\nScreenshot saved to {}\nSize: 1KB\n",
            png.display()
        );
        let (_text, imgs) = augment_tool_output_with_screenshot_images(stdout);
        let _ = std::fs::remove_file(&png);
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].media_type, "image/png");
        assert!(!imgs[0].data_base64.is_empty());
    }
}
