use super::*;

pub(crate) async fn cmd_viewport(
    ctx: &mut AppContext,
    width: Option<&str>,
    height: Option<&str>,
) -> Result<()> {
    let width = width.ok_or_else(|| {
        anyhow!(
            "Usage: sidekar viewport <width> <height>\nPresets: mobile, tablet, desktop, iphone, ipad"
        )
    })?;
    let (w, h, dpr, mobile) = match width.to_lowercase().as_str() {
        "mobile" => (375i64, 667i64, 1i64, true),
        "iphone" => (390, 844, 1, true),
        "ipad" => (820, 1180, 1, true),
        "tablet" => (768, 1024, 1, true),
        "desktop" => (1280, 800, 1, false),
        _ => {
            let w = width
                .parse::<i64>()
                .context("Invalid width. Use a number or preset.")?;
            let h = height
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or((w as f64 * 0.625).round() as i64);
            (w, h, 1, false)
        }
    };
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    cdp.send(
        "Emulation.setDeviceMetricsOverride",
        json!({
            "width": w,
            "height": h,
            "deviceScaleFactor": dpr,
            "mobile": mobile
        }),
    )
    .await?;
    out!(
        ctx,
        "Viewport set to {}x{} (dpr:{}{})",
        w,
        h,
        dpr,
        if mobile { " mobile" } else { "" }
    );
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_zoom(ctx: &mut AppContext, level: Option<&str>) -> Result<()> {
    let mut state = ctx.load_session_state()?;
    let current = state.zoom_level.unwrap_or(100.0);

    let new_level = match level.unwrap_or("") {
        "in" => (current + 25.0).min(200.0),
        "out" => (current - 25.0).max(25.0),
        "reset" | "" => 100.0,
        v => v
            .parse::<f64>()
            .context("Usage: sidekar zoom <in|out|reset|25-200>")?,
    };

    let new_level = new_level.clamp(25.0, 200.0);
    let zoom_factor = new_level / 100.0;

    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;

    let script = format!("document.documentElement.style.zoom = '{zoom_factor}';");
    runtime_evaluate(&mut cdp, &script, true, false).await?;

    state.zoom_level = if (new_level - 100.0).abs() < 0.01 {
        None
    } else {
        Some(new_level)
    };
    ctx.save_session_state(&state)?;

    out!(ctx, "Zoom: {}%", new_level as u32);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_frames(ctx: &mut AppContext) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    cdp.send("Page.enable", json!({})).await?;
    let tree = cdp.send("Page.getFrameTree", json!({})).await?;
    print_frame_tree(
        &mut ctx.output,
        tree.get("frameTree").unwrap_or(&Value::Null),
        0,
    );
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_frame(
    ctx: &mut AppContext,
    frame_id_or_selector: Option<&str>,
) -> Result<()> {
    let frame_id_or_selector = frame_id_or_selector.ok_or_else(|| {
        anyhow!(
            "Usage: sidekar frame <frameId|selector>\nUse \"sidekar frames\" to list frames.\nUse \"sidekar frame main\" to return to main frame."
        )
    })?;
    let mut state = ctx.load_session_state()?;
    if matches!(frame_id_or_selector, "main" | "top") {
        state.active_frame_id = None;
        ctx.save_session_state(&state)?;
        out!(ctx, "Switched to main frame.");
        return Ok(());
    }

    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    cdp.send("Page.enable", json!({})).await?;
    let tree = cdp.send("Page.getFrameTree", json!({})).await?;

    let mut found = find_frame_in_tree(
        tree.get("frameTree").unwrap_or(&Value::Null),
        frame_id_or_selector,
    );

    if found.is_none() {
        let info = runtime_evaluate(
            &mut cdp,
            &format!(
                r#"(function() {{
                const el = document.querySelector({sel});
                if (!el || (el.tagName !== 'IFRAME' && el.tagName !== 'FRAME')) return null;
                return {{ name: el.getAttribute('name') || null, id: el.id || null, src: el.src || null }};
              }})()"#,
                sel = serde_json::to_string(frame_id_or_selector)?
            ),
            true,
            false,
        )
        .await?;
        let info_val = info
            .pointer("/result/value")
            .cloned()
            .unwrap_or(Value::Null);
        if !info_val.is_null() {
            if let Some(name_or_id) = info_val
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| info_val.get("id").and_then(Value::as_str))
            {
                found =
                    find_frame_in_tree(tree.get("frameTree").unwrap_or(&Value::Null), name_or_id);
            }
            if found.is_none() {
                if let Some(src) = info_val.get("src").and_then(Value::as_str) {
                    found = find_frame_by_url(tree.get("frameTree").unwrap_or(&Value::Null), src);
                }
            }
        }
    }

    let frame = found.ok_or_else(|| anyhow!("Frame not found: {}", frame_id_or_selector))?;
    state.active_frame_id = Some(frame.0.clone());
    ctx.save_session_state(&state)?;
    out!(ctx, "Switched to frame: [{}] {}", frame.0, frame.1);
    cdp.close().await;
    Ok(())
}
