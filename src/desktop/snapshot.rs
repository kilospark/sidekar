//! Desktop snapshot: capture screenshot + interactive element map (+ optional annotation).

use super::interact;
use super::types::*;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub async fn build_see_snapshot(
    pid: i32,
    tmp_dir: &Path,
    annotate: bool,
    target_width: Option<u32>,
    max_depth: usize,
    max_elements: usize,
) -> Result<DesktopSeeSnapshot> {
    let snapshot_id = format!(
        "snap-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let raw_png = tmp_dir.join(format!("{snapshot_id}-raw.png"));
    super::screen::capture_desktop_screenshot(Some(pid), &raw_png).await?;

    let elements = interact::snapshot_interactive_elements(pid, max_depth, max_elements)?;

    let mut img = image::open(&raw_png)
        .with_context(|| format!("failed to read screenshot {}", raw_png.display()))?;
    let _ = std::fs::remove_file(&raw_png);

    if annotate {
        annotate_elements(&mut img, &elements);
    }

    let target_w = target_width.unwrap_or(800).min(img.width());
    if target_w < img.width() {
        img = img.resize(target_w, u32::MAX, image::imageops::FilterType::Lanczos3);
    }

    let screenshot_path = tmp_dir.join(format!("{snapshot_id}.jpg"));
    let mut out_file = std::fs::File::create(&screenshot_path)
        .with_context(|| format!("failed to create {}", screenshot_path.display()))?;
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out_file, 80);
    img.write_with_encoder(encoder)
        .context("failed to encode annotated screenshot")?;

    let annotated_path = if annotate {
        Some(screenshot_path.to_string_lossy().into_owned())
    } else {
        None
    };

    Ok(DesktopSeeSnapshot {
        snapshot_id,
        pid,
        element_count: elements.len(),
        screenshot_path: screenshot_path.to_string_lossy().into_owned(),
        annotated_path,
        elements,
    })
}

fn annotate_elements(img: &mut image::DynamicImage, elements: &[DesktopElementMatch]) {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut buf = rgba;
    for el in elements {
        let Some(frame) = &el.frame else { continue };
        let x0 = frame.x.max(0.0) as u32;
        let y0 = frame.y.max(0.0) as u32;
        let x1 = ((frame.x + frame.width) as u32).min(w.saturating_sub(1));
        let y1 = ((frame.y + frame.height) as u32).min(h.saturating_sub(1));
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        draw_rect_border(&mut buf, x0, y0, x1, y1, [0, 180, 255, 255]);
        let label = el
            .ref_id
            .clone()
            .or_else(|| el.title.clone())
            .unwrap_or_else(|| el.role.clone());
        draw_label(&mut buf, x0, y0.saturating_sub(10), &label);
    }
    *img = image::DynamicImage::ImageRgba8(buf);
}

fn draw_rect_border(
    buf: &mut image::RgbaImage,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    color: [u8; 4],
) {
    for x in x0..=x1 {
        if let Some(p) = buf.get_pixel_mut_checked(x, y0) {
            *p = image::Rgba(color);
        }
        if let Some(p) = buf.get_pixel_mut_checked(x, y1) {
            *p = image::Rgba(color);
        }
    }
    for y in y0..=y1 {
        if let Some(p) = buf.get_pixel_mut_checked(x0, y) {
            *p = image::Rgba(color);
        }
        if let Some(p) = buf.get_pixel_mut_checked(x1, y) {
            *p = image::Rgba(color);
        }
    }
}

fn draw_label(buf: &mut image::RgbaImage, x: u32, y: u32, text: &str) {
    let max = text.chars().take(12).collect::<String>();
    for (i, _) in max.chars().enumerate() {
        let px = x.saturating_add(i as u32 * 6);
        if px + 5 >= buf.width() || y + 5 >= buf.height() {
            break;
        }
        for dy in 0..5 {
            for dx in 0..5 {
                if let Some(p) = buf.get_pixel_mut_checked(px + dx, y + dy) {
                    *p = image::Rgba([20, 20, 20, 220]);
                }
            }
        }
    }
}

pub fn snapshot_meta_path(snapshot_id: &str) -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".sidekar")
        .join("desktop-snapshots")
        .join(format!("{snapshot_id}.json"))
}

pub fn persist_snapshot(snapshot: &DesktopSeeSnapshot) -> Result<()> {
    let path = snapshot_meta_path(&snapshot.snapshot_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(snapshot)?)?;
    Ok(())
}
