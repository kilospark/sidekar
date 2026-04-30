//! Diagnostic commands. Not part of the stable CLI surface — these exist to
//! pin down environmental quirks like coordinate-system discrepancies.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::sleep;

use crate::AppContext;
use crate::output::PlainOutput;
use crate::{open_cdp, prepare_cdp};

pub(crate) async fn cmd_debug(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "click-probe" => click_probe(ctx, &args[1..]).await,
        "" => bail!("Usage: sidekar debug <click-probe> [args]"),
        other => bail!("Unknown debug subcommand: {other}"),
    }
}

async fn click_probe(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let (x, y) = crate::utils::parse_coordinates(args).unwrap_or((200.0, 360.0));
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;

    // Install a document-level click listener that records every click.
    let install = r#"
        (() => {
          window.__sk_probe = [];
          if (window.__sk_probe_listener) {
            document.removeEventListener('click', window.__sk_probe_listener, true);
          }
          window.__sk_probe_listener = (e) => {
            window.__sk_probe.push({
              t: e.target ? e.target.tagName : '?',
              isTrusted: e.isTrusted,
              cx: e.clientX,
              cy: e.clientY,
              sx: e.screenX,
              sy: e.screenY,
              sc: e.sourceCapabilities ? e.sourceCapabilities.firesTouchEvents : null
            });
          };
          document.addEventListener('click', window.__sk_probe_listener, true);
          return 'armed';
        })()
    "#;
    let _ = crate::browser::runtime_evaluate(&mut cdp, install, true, false)
        .await
        .with_context(|| "failed to install probe listener")?;

    // Measure window metrics via the helper we already have.
    let metrics = crate::browser::os_click::measure_window(&mut cdp).await?;

    // Fire CDP click at (x, y).
    cdp.send(
        "Input.dispatchMouseEvent",
        json!({ "type": "mouseMoved", "x": x, "y": y }),
    )
    .await?;
    sleep(Duration::from_millis(80)).await;
    cdp.send(
        "Input.dispatchMouseEvent",
        json!({ "type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1 }),
    )
    .await?;
    cdp.send(
        "Input.dispatchMouseEvent",
        json!({ "type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1 }),
    )
    .await?;
    sleep(Duration::from_millis(200)).await;

    let cdp_result = read_probe(&mut cdp).await?;

    // Fire OS click at the same CSS coord.
    let os_result = match crate::browser::os_click::os_click_css(ctx, &mut cdp, x, y).await {
        Ok((cx, cy, _m)) => {
            sleep(Duration::from_millis(400)).await;
            let page = read_probe(&mut cdp).await?;
            OsProbeResult {
                ok: true,
                passed_x: cx,
                passed_y: cy,
                page_events: page,
                error: None,
            }
        }
        Err(e) => OsProbeResult {
            ok: false,
            passed_x: 0.0,
            passed_y: 0.0,
            page_events: Vec::new(),
            error: Some(e.to_string()),
        },
    };

    let window_suspect = metrics.screen_x == 0.0
        && metrics.screen_y == 0.0
        && metrics.chrome_top == 0.0
        && metrics.chrome_left == 0.0;
    let suspect_note = if window_suspect {
        "\n⚠  window reports 0 outer dims — likely not foreground. CGEvent click may land elsewhere. Bring window to front and rerun.\n"
    } else {
        ""
    };

    let report = format!(
        "click-probe @ CSS ({x}, {y})\n\
         {suspect}\n\
         window metrics (from page):\n\
           screenXY       = ({:.0}, {:.0})\n\
           chrome inset   = ({:.0} left, {:.0} top)\n\
           devicePixelRatio = {}\n\
           visualViewport.scale = {}\n\
         \n\
         CDP click result:\n\
           events captured: {}\n\
           last event:      {}\n\
         \n\
         OS click result:\n\
           ok:              {}\n\
           passed to enigo: ({}, {})\n\
           events captured: {}\n\
           last event:      {}\n\
           error:           {}",
        metrics.screen_x,
        metrics.screen_y,
        metrics.chrome_left,
        metrics.chrome_top,
        metrics.dpr,
        metrics.page_zoom,
        cdp_result.len(),
        format_last(&cdp_result),
        os_result.ok,
        os_result.passed_x,
        os_result.passed_y,
        os_result.page_events.len(),
        format_last(&os_result.page_events),
        os_result.error.as_deref().unwrap_or("(none)"),
        suspect = suspect_note,
    );
    out!(
        ctx,
        "{}",
        crate::output::to_string(&PlainOutput::new(report))?
    );
    Ok(())
}

struct OsProbeResult {
    ok: bool,
    passed_x: f64,
    passed_y: f64,
    page_events: Vec<Value>,
    error: Option<String>,
}

async fn read_probe(cdp: &mut crate::cdp::CdpClient) -> Result<Vec<Value>> {
    let script = "JSON.stringify(window.__sk_probe || [])";
    let resp = crate::browser::runtime_evaluate(cdp, script, true, false).await?;
    let raw = resp
        .pointer("/result/value")
        .and_then(|v| v.as_str())
        .unwrap_or("[]");
    serde_json::from_str(raw).context("probe result parse")
}

fn format_last(events: &[Value]) -> String {
    match events.last() {
        Some(v) => v.to_string(),
        None => "(no click received)".to_string(),
    }
}
