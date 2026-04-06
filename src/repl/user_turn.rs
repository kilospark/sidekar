//! Build user `ContentBlock`s from REPL input (Codex-style path paste vs image attach).

use std::path::{Path, PathBuf};

use anyhow::Context;
use base64::Engine;
use regex::Regex;
use url::Url;

use crate::providers::ContentBlock;

/// Cap per attached image file (bytes).
const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;

/// Normalize a single pasted path (file URL, quotes, shell-escaped, Windows paths on Unix).
pub fn normalize_pasted_path(pasted: &str) -> Option<PathBuf> {
    let pasted = pasted.trim();
    if pasted.is_empty() || pasted.contains('\n') {
        return None;
    }
    let unquoted = pasted
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| pasted.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(pasted);

    if let Ok(url) = Url::parse(unquoted)
        && url.scheme() == "file"
    {
        return url.to_file_path().ok();
    }

    let parts: Vec<String> = shlex::Shlex::new(pasted).collect();
    if parts.len() == 1 {
        return Some(PathBuf::from(parts.into_iter().next()?));
    }

    None
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
        _ => "image/png",
    }
}

fn read_image_block(path: &Path) -> anyhow::Result<ContentBlock> {
    let meta = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if meta.len() > MAX_IMAGE_BYTES {
        anyhow::bail!(
            "image too large (max {} MiB)",
            MAX_IMAGE_BYTES / 1024 / 1024
        );
    }
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let media_type = guess_media_type(path).to_string();
    let data_base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(ContentBlock::Image {
        media_type,
        data_base64,
        source_path: Some(path.to_string_lossy().to_string()),
    })
}

/// Materialize base64-only images to temp files, then add path lines before each inline image.
pub fn finalize_multimodal_for_api(blocks: &mut Vec<ContentBlock>) -> anyhow::Result<()> {
    crate::agent::images::materialize_ephemeral_image_sources(blocks)?;
    crate::agent::images::affix_image_path_citations(blocks);
    Ok(())
}

/// Turn REPL buffer text plus ordered attachment paths into API content blocks.
pub fn build_user_turn_content(
    text: &str,
    ordered_paths: &[PathBuf],
) -> anyhow::Result<Vec<ContentBlock>> {
    let trimmed = text.trim();

    if ordered_paths.is_empty() {
        if trimmed.is_empty() {
            return Ok(vec![]);
        }
        if !trimmed.contains('\n') {
            if let Some(pb) = normalize_pasted_path(trimmed) {
                if std::fs::metadata(&pb).map(|m| m.len()).unwrap_or(0) <= MAX_IMAGE_BYTES
                    && image::image_dimensions(&pb).is_ok()
                {
                    let img = read_image_block(&pb)?;
                    return Ok(vec![
                        ContentBlock::Text {
                            text: format!("Attached image: {}", pb.display()),
                        },
                        img,
                    ]);
                }
            }
        }
        return Ok(vec![ContentBlock::Text {
            text: text.to_string(),
        }]);
    }

    let re = Regex::new(r"\[Image #(\d+)\]").expect("valid regex");
    if !re.is_match(text) {
        return Ok(vec![ContentBlock::Text {
            text: text.to_string(),
        }]);
    }

    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut last = 0usize;
    for cap in re.captures_iter(text) {
        let m = cap.get(0).unwrap();
        let before = text[last..m.start()].to_string();
        if !before.is_empty() {
            blocks.push(ContentBlock::Text { text: before });
        }
        let idx: usize = cap[1]
            .parse()
            .context("invalid [Image #N] index")?;
        if idx == 0 || idx > ordered_paths.len() {
            anyhow::bail!(
                "[Image #{idx}] does not match an attachment (you have {} attached)",
                ordered_paths.len()
            );
        }
        blocks.push(read_image_block(&ordered_paths[idx - 1])?);
        last = m.end();
    }
    let tail = text[last..].to_string();
    if !tail.is_empty() {
        blocks.push(ContentBlock::Text { text: tail });
    }

    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_image_path_yields_image_block() {
        let png = std::env::temp_dir().join(format!(
            "sidekar_repl_img_test_{}.png",
            std::process::id()
        ));
        let one_pixel: [u8; 67] = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        std::fs::write(&png, one_pixel.as_slice()).unwrap();
        let path_str = png.to_str().unwrap();
        let blocks = build_user_turn_content(path_str, &[]).unwrap();
        assert_eq!(blocks.len(), 2);
        assert!(matches!(blocks[0], ContentBlock::Text { .. }));
        assert!(matches!(blocks[1], ContentBlock::Image { .. }));
        let _ = std::fs::remove_file(&png);
    }
}
