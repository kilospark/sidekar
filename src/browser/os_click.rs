//! Derived-calibration OS-level click for browser targets.
//!
//! The click coordinates coming out of `observe` / `ax-tree` / `find` are in
//! CSS viewport pixels. CGEvent needs screen pixels. The gap between them is
//! the Chrome window origin + the Chrome UI inset (title bar + tab strip +
//! address bar) + the debugger infobar if the debugger is attached + any CSS
//! zoom. This module measures those at call time rather than baking magic
//! constants, since they vary by OS theme, HiDPI, zoom level, and whether a
//! CDP attach left a banner up.
//!
//! The translation lives here in the browser crate because it needs a live
//! CDP client to eval in the page; the raw CGEvent post goes through
//! `desktop::bg_input::click_at_pid` (frontmost HID tap path when no pid).

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::AppContext;
use crate::cdp::CdpClient;

/// Metrics measured from the page right before posting the click.
#[derive(Debug, Clone, Copy)]
pub struct WindowMetrics {
    /// Browser window origin on the virtual screen, in points (CGEvent space).
    pub screen_x: f64,
    pub screen_y: f64,
    /// Horizontal chrome inset = outerWidth - innerWidth. On macOS usually
    /// ~0 because window controls are in the title bar, not on the sides.
    pub chrome_left: f64,
    /// Vertical chrome inset = outerHeight - innerHeight. Includes title bar
    /// + tab strip + address bar + debugger banner (when attached).
    pub chrome_top: f64,
    /// devicePixelRatio. CGEvent on macOS takes points, not pixels, so we do
    /// not multiply screen coords by DPR; carried here for diagnostics only.
    pub dpr: f64,
    /// Page zoom as reported to the page (not the same as sidekar's saved
    /// zoom_level which is the CSS-zoom factor we injected).
    pub page_zoom: f64,
}

const METRICS_EXPR: &str = r#"
(() => {
  try {
    const vv = window.visualViewport || null;
    return JSON.stringify({
      screenX: window.screenX,
      screenY: window.screenY,
      outerW: window.outerWidth,
      outerH: window.outerHeight,
      innerW: window.innerWidth,
      innerH: window.innerHeight,
      dpr: window.devicePixelRatio || 1,
      vvOffsetLeft: vv ? vv.offsetLeft : 0,
      vvOffsetTop: vv ? vv.offsetTop : 0,
      vvScale: vv ? vv.scale : 1,
    });
  } catch (e) {
    return JSON.stringify({ error: String(e) });
  }
})()
"#;

/// Measure window metrics via CDP Runtime.evaluate.
pub async fn measure_window(cdp: &mut CdpClient) -> Result<WindowMetrics> {
    let resp = cdp
        .send(
            "Runtime.evaluate",
            json!({
                "expression": METRICS_EXPR,
                "returnByValue": true,
                "awaitPromise": false,
            }),
        )
        .await?;

    let raw = resp
        .pointer("/result/value")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("page metrics probe returned no value"))?;
    let parsed: Value = serde_json::from_str(raw)
        .map_err(|e| anyhow!("page metrics probe returned non-JSON: {e}"))?;
    if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
        return Err(anyhow!("page metrics probe threw: {err}"));
    }

    let g = |k: &str| parsed.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let outer_w = g("outerW");
    let outer_h = g("outerH");
    let inner_w = g("innerW");
    let inner_h = g("innerH");
    let chrome_left = ((outer_w - inner_w) * 0.5).max(0.0);
    let chrome_top = (outer_h - inner_h).max(0.0);

    Ok(WindowMetrics {
        screen_x: g("screenX"),
        screen_y: g("screenY"),
        chrome_left,
        chrome_top,
        dpr: parsed.get("dpr").and_then(|v| v.as_f64()).unwrap_or(1.0),
        page_zoom: parsed
            .get("vvScale")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0),
    })
}

/// Translate a CSS-viewport coordinate to CGEvent screen coordinates.
///
/// Input (css_x, css_y) must already be in the *viewport* coordinate space —
/// the output of `adjust_coords_for_zoom` (which inverts CSS zoom) is the
/// right feed point. This function does not undo zoom itself.
pub fn to_screen(css_x: f64, css_y: f64, m: &WindowMetrics) -> (f64, f64) {
    let sx = m.screen_x + m.chrome_left + css_x;
    let sy = m.screen_y + m.chrome_top + css_y;
    (sx, sy)
}

/// Click at a page CSS viewport coordinate via OS-level CGEvent input.
///
/// Goes through `desktop::bg_input::click_at_pid` → CGEvent at kCGHIDEventTap
/// (frontmost path), which produces an `event.isTrusted === true` click
/// indistinguishable from the user's own mouse.
///
/// Empirical note (macOS + Chrome): `click_at(x, y)` on this combination
/// lands at CSS viewport (x, y) of the focused window, not at raw screen
/// (x, y). We pass CSS viewport coords directly. The `measure_*` helpers
/// stay available for Linux/Windows where coord systems differ and for
/// diagnostic reporting.
pub async fn os_click_css(
    ctx: &AppContext,
    cdp: &mut CdpClient,
    css_x: f64,
    css_y: f64,
) -> Result<(f64, f64, WindowMetrics)> {
    let (zx, zy) = crate::utils::adjust_coords_for_zoom(ctx, css_x, css_y);
    let metrics = measure_window(cdp).await?;
    #[cfg(target_os = "macos")]
    {
        crate::desktop::bg_input::click_frontmost(
            zx,
            zy,
            crate::desktop::bg_input::MouseButton::Left,
            1,
        )?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        return Err(anyhow!(
            "--os click not yet supported on this platform (macOS only); see context/todo.md"
        ));
    }
    Ok((zx, zy, metrics))
}
