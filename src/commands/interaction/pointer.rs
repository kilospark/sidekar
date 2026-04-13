use super::*;
use crate::output::PlainOutput;

#[derive(serde::Serialize)]
struct PointerActionOutput {
    action_line: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    adopted_line: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page_brief: Option<String>,
}

impl crate::output::CommandOutput for PointerActionOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "{}", self.action_line)?;
        if let Some(a) = &self.adopted_line {
            writeln!(w, "{a}")?;
        }
        if let Some(b) = &self.page_brief {
            writeln!(w, "{b}")?;
        }
        Ok(())
    }
}

pub(crate) async fn cmd_click_dispatch(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if let Some((raw_x, raw_y)) = parse_coordinates(args) {
        let (x, y) = adjust_coords_for_zoom(ctx, raw_x, raw_y);
        let tabs_before = snapshot_tab_ids(ctx).await?;
        let mut cdp = open_cdp(ctx).await?;
        prepare_cdp(ctx, &mut cdp).await?;
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
        let action_line = format!("Clicked at ({x}, {y})");
        sleep(Duration::from_millis(150)).await;
        let adopted = adopt_new_tabs(ctx, &tabs_before, Duration::from_millis(800)).await?;
        if !adopted.is_empty() {
            cdp.close().await;
            let mut adopted_cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut adopted_cdp).await?;
            let adopted_line = format!(
                "Adopted {} new tab(s); switched to [{}]",
                adopted.len(),
                adopted
                    .iter()
                    .find(|tab| tab.url.as_deref().is_some_and(|url| url != "about:blank"))
                    .or_else(|| adopted.first())
                    .map(|tab| tab.id.as_str())
                    .unwrap_or("unknown")
            );
            let page_brief = get_page_brief(&mut adopted_cdp).await?;
            adopted_cdp.close().await;
            let output = PointerActionOutput {
                action_line,
                adopted_line: Some(adopted_line),
                page_brief: Some(page_brief),
            };
            out!(ctx, "{}", crate::output::to_string(&output)?);
            return Ok(());
        }
        let page_brief = get_page_brief(&mut cdp).await?;
        cdp.close().await;
        let output = PointerActionOutput {
            action_line,
            adopted_line: None,
            page_brief: Some(page_brief),
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--text") {
        let text = args[1..].join(" ");
        if text.is_empty() {
            bail!("Usage: sidekar click --text <text>");
        }
        let tabs_before = snapshot_tab_ids(ctx).await?;
        let mut cdp = open_cdp(ctx).await?;
        prepare_cdp(ctx, &mut cdp).await?;
        let loc = locate_element_by_text(ctx, &mut cdp, &text).await;
        match loc {
            Ok(loc) => {
                cdp.send(
                    "Input.dispatchMouseEvent",
                    json!({ "type": "mouseMoved", "x": loc.x, "y": loc.y }),
                )
                .await?;
                sleep(Duration::from_millis(80)).await;
                cdp.send(
                    "Input.dispatchMouseEvent",
                    json!({ "type": "mousePressed", "x": loc.x, "y": loc.y, "button": "left", "clickCount": 1 }),
                )
                .await?;
                cdp.send(
                    "Input.dispatchMouseEvent",
                    json!({ "type": "mouseReleased", "x": loc.x, "y": loc.y, "button": "left", "clickCount": 1 }),
                )
                .await?;
                let action_line = format!(
                    "Clicked {} \"{}\" (text match)",
                    loc.tag.to_lowercase(),
                    loc.text
                );
                sleep(Duration::from_millis(150)).await;
                let adopted = adopt_new_tabs(ctx, &tabs_before, Duration::from_millis(800)).await?;
                if !adopted.is_empty() {
                    cdp.close().await;
                    let mut adopted_cdp = open_cdp(ctx).await?;
                    prepare_cdp(ctx, &mut adopted_cdp).await?;
                    let adopted_line = format!(
                        "Adopted {} new tab(s); switched to [{}]",
                        adopted.len(),
                        adopted
                            .iter()
                            .find(|tab| tab.url.as_deref().is_some_and(|url| url != "about:blank"))
                            .or_else(|| adopted.first())
                            .map(|tab| tab.id.as_str())
                            .unwrap_or("unknown")
                    );
                    let page_brief = get_page_brief(&mut adopted_cdp).await?;
                    adopted_cdp.close().await;
                    let output = PointerActionOutput {
                        action_line,
                        adopted_line: Some(adopted_line),
                        page_brief: Some(page_brief),
                    };
                    out!(ctx, "{}", crate::output::to_string(&output)?);
                    return Ok(());
                }
                let page_brief = get_page_brief(&mut cdp).await?;
                cdp.close().await;
                let output = PointerActionOutput {
                    action_line,
                    adopted_line: None,
                    page_brief: Some(page_brief),
                };
                out!(ctx, "{}", crate::output::to_string(&output)?);
                return Ok(());
            }
            Err(browser_err) => {
                cdp.close().await;
                // Try desktop accessibility fallback (macOS only)
                if let Some(ref browser) = ctx.launch_browser_name
                    && let Ok(msg) =
                        super::super::desktop::try_desktop_click_fallback(browser, &text)
                {
                    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
                    return Ok(());
                }
                return Err(browser_err);
            }
        }
    }
    let selector = resolve_selector(ctx, &args.join(" "))?;
    match cmd_click(ctx, &selector).await {
        Ok(()) => Ok(()),
        Err(browser_err) => {
            // Try desktop accessibility fallback (macOS only)
            if let Some(ref browser) = ctx.launch_browser_name
                && let Ok(msg) =
                    super::super::desktop::try_desktop_click_fallback(browser, &selector)
            {
                out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
                return Ok(());
            }
            Err(browser_err)
        }
    }
}

pub(crate) async fn cmd_double_click_dispatch(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if let Some((raw_x, raw_y)) = parse_coordinates(args) {
        let (x, y) = adjust_coords_for_zoom(ctx, raw_x, raw_y);
        let mut cdp = open_cdp(ctx).await?;
        prepare_cdp(ctx, &mut cdp).await?;
        dispatch_double_click(&mut cdp, x, y).await?;
        let action_line = format!("Double-clicked at ({x}, {y})");
        sleep(Duration::from_millis(150)).await;
        let page_brief = get_page_brief(&mut cdp).await?;
        cdp.close().await;
        let output = PointerActionOutput {
            action_line,
            adopted_line: None,
            page_brief: Some(page_brief),
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--text") {
        let text = args[1..].join(" ");
        if text.is_empty() {
            bail!("Usage: sidekar click --mode=double --text <text>");
        }
        let mut cdp = open_cdp(ctx).await?;
        prepare_cdp(ctx, &mut cdp).await?;
        let loc = locate_element_by_text(ctx, &mut cdp, &text).await?;
        dispatch_double_click(&mut cdp, loc.x, loc.y).await?;
        let action_line = format!(
            "Double-clicked {} \"{}\" (text match)",
            loc.tag.to_lowercase(),
            loc.text
        );
        sleep(Duration::from_millis(150)).await;
        let page_brief = get_page_brief(&mut cdp).await?;
        cdp.close().await;
        let output = PointerActionOutput {
            action_line,
            adopted_line: None,
            page_brief: Some(page_brief),
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
        return Ok(());
    }
    let selector = resolve_selector(ctx, &args.join(" "))?;
    cmd_double_click(ctx, &selector).await
}

pub(crate) async fn cmd_right_click_dispatch(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if let Some((raw_x, raw_y)) = parse_coordinates(args) {
        let (x, y) = adjust_coords_for_zoom(ctx, raw_x, raw_y);
        let mut cdp = open_cdp(ctx).await?;
        prepare_cdp(ctx, &mut cdp).await?;
        dispatch_right_click(&mut cdp, x, y).await?;
        let action_line = format!("Right-clicked at ({x}, {y})");
        sleep(Duration::from_millis(150)).await;
        let page_brief = get_page_brief(&mut cdp).await?;
        cdp.close().await;
        let output = PointerActionOutput {
            action_line,
            adopted_line: None,
            page_brief: Some(page_brief),
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--text") {
        let text = args[1..].join(" ");
        if text.is_empty() {
            bail!("Usage: sidekar click --mode=right --text <text>");
        }
        let mut cdp = open_cdp(ctx).await?;
        prepare_cdp(ctx, &mut cdp).await?;
        let loc = locate_element_by_text(ctx, &mut cdp, &text).await?;
        dispatch_right_click(&mut cdp, loc.x, loc.y).await?;
        let action_line = format!(
            "Right-clicked {} \"{}\" (text match)",
            loc.tag.to_lowercase(),
            loc.text
        );
        sleep(Duration::from_millis(150)).await;
        let page_brief = get_page_brief(&mut cdp).await?;
        cdp.close().await;
        let output = PointerActionOutput {
            action_line,
            adopted_line: None,
            page_brief: Some(page_brief),
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
        return Ok(());
    }
    let selector = resolve_selector(ctx, &args.join(" "))?;
    cmd_right_click(ctx, &selector).await
}

pub(crate) async fn cmd_hover_dispatch(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if let Some((raw_x, raw_y)) = parse_coordinates(args) {
        let (x, y) = adjust_coords_for_zoom(ctx, raw_x, raw_y);
        let mut cdp = open_cdp(ctx).await?;
        prepare_cdp(ctx, &mut cdp).await?;
        cdp.send(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": x, "y": y }),
        )
        .await?;
        let action_line = format!("Hovered at ({x}, {y})");
        sleep(Duration::from_millis(150)).await;
        let page_brief = get_page_brief(&mut cdp).await?;
        cdp.close().await;
        let output = PointerActionOutput {
            action_line,
            adopted_line: None,
            page_brief: Some(page_brief),
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--text") {
        let text = args[1..].join(" ");
        if text.is_empty() {
            bail!("Usage: sidekar hover --text <text>");
        }
        let mut cdp = open_cdp(ctx).await?;
        prepare_cdp(ctx, &mut cdp).await?;
        let loc = locate_element_by_text(ctx, &mut cdp, &text).await?;
        cdp.send(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": loc.x, "y": loc.y }),
        )
        .await?;
        let action_line = format!(
            "Hovered {} \"{}\" (text match)",
            loc.tag.to_lowercase(),
            loc.text
        );
        sleep(Duration::from_millis(150)).await;
        let page_brief = get_page_brief(&mut cdp).await?;
        cdp.close().await;
        let output = PointerActionOutput {
            action_line,
            adopted_line: None,
            page_brief: Some(page_brief),
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
        return Ok(());
    }
    let selector = resolve_selector(ctx, &args.join(" "))?;
    cmd_hover(ctx, &selector).await
}

pub(crate) async fn cmd_double_click(ctx: &mut AppContext, selector: &str) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let loc = locate_element(ctx, &mut cdp, selector).await?;
    dispatch_double_click(&mut cdp, loc.x, loc.y).await?;
    let action_line = format!(
        "Double-clicked {} \"{}\"",
        loc.tag.to_lowercase(),
        loc.text
    );
    sleep(Duration::from_millis(150)).await;
    let page_brief = get_page_brief(&mut cdp).await?;
    cdp.close().await;
    let output = PointerActionOutput {
        action_line,
        adopted_line: None,
        page_brief: Some(page_brief),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

pub(crate) async fn cmd_right_click(ctx: &mut AppContext, selector: &str) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let loc = locate_element(ctx, &mut cdp, selector).await?;
    dispatch_right_click(&mut cdp, loc.x, loc.y).await?;
    let action_line = format!(
        "Right-clicked {} \"{}\"",
        loc.tag.to_lowercase(),
        loc.text
    );
    sleep(Duration::from_millis(150)).await;
    let page_brief = get_page_brief(&mut cdp).await?;
    cdp.close().await;
    let output = PointerActionOutput {
        action_line,
        adopted_line: None,
        page_brief: Some(page_brief),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

pub(crate) async fn cmd_hover(ctx: &mut AppContext, selector: &str) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let loc = locate_element(ctx, &mut cdp, selector).await?;
    cdp.send(
        "Input.dispatchMouseEvent",
        json!({ "type": "mouseMoved", "x": loc.x, "y": loc.y }),
    )
    .await?;
    let action_line = format!("Hovered {} \"{}\"", loc.tag.to_lowercase(), loc.text);
    sleep(Duration::from_millis(150)).await;
    let page_brief = get_page_brief(&mut cdp).await?;
    cdp.close().await;
    let output = PointerActionOutput {
        action_line,
        adopted_line: None,
        page_brief: Some(page_brief),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

pub(crate) async fn cmd_drag(
    ctx: &mut AppContext,
    from_selector: &str,
    to_selector: &str,
) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let from = locate_element(ctx, &mut cdp, from_selector).await?;
    let to = locate_element(ctx, &mut cdp, to_selector).await?;
    cdp.send(
        "Input.dispatchMouseEvent",
        json!({ "type": "mouseMoved", "x": from.x, "y": from.y }),
    )
    .await?;
    cdp.send(
        "Input.dispatchMouseEvent",
        json!({ "type": "mousePressed", "x": from.x, "y": from.y, "button": "left", "clickCount": 1 }),
    )
    .await?;
    for i in 1..=5 {
        let x = from.x + (to.x - from.x) * (i as f64 / 5.0);
        let y = from.y + (to.y - from.y) * (i as f64 / 5.0);
        cdp.send(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": x, "y": y }),
        )
        .await?;
    }
    cdp.send(
        "Input.dispatchMouseEvent",
        json!({ "type": "mouseReleased", "x": to.x, "y": to.y, "button": "left", "clickCount": 1 }),
    )
    .await?;
    let action_line = format!(
        "Dragged {} to {}",
        from.tag.to_lowercase(),
        to.tag.to_lowercase()
    );
    let page_brief = get_page_brief(&mut cdp).await?;
    cdp.close().await;
    let output = PointerActionOutput {
        action_line,
        adopted_line: None,
        page_brief: Some(page_brief),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

pub(crate) async fn dispatch_double_click(cdp: &mut CdpClient, x: f64, y: f64) -> Result<()> {
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
    cdp.send(
        "Input.dispatchMouseEvent",
        json!({ "type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 2 }),
    )
    .await?;
    cdp.send(
        "Input.dispatchMouseEvent",
        json!({ "type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 2 }),
    )
    .await?;
    Ok(())
}

pub(crate) async fn dispatch_right_click(cdp: &mut CdpClient, x: f64, y: f64) -> Result<()> {
    cdp.send(
        "Input.dispatchMouseEvent",
        json!({ "type": "mousePressed", "x": x, "y": y, "button": "right", "clickCount": 1 }),
    )
    .await?;
    cdp.send(
        "Input.dispatchMouseEvent",
        json!({ "type": "mouseReleased", "x": x, "y": y, "button": "right", "clickCount": 1 }),
    )
    .await?;
    Ok(())
}

// --- Raw mouse primitives ---

pub(crate) async fn cmd_mouse(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("");
    match action {
        "move" => {
            let x: f64 = args
                .get(1)
                .and_then(|v| v.parse().ok())
                .context("Usage: mouse move <x> <y>")?;
            let y: f64 = args
                .get(2)
                .and_then(|v| v.parse().ok())
                .context("Usage: mouse move <x> <y>")?;
            let (x, y) = adjust_coords_for_zoom(ctx, x, y);
            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;
            cdp.send(
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseMoved", "x": x, "y": y }),
            )
            .await?;
            let mut state = ctx.load_session_state()?;
            state.mouse_x = Some(x);
            state.mouse_y = Some(y);
            ctx.save_session_state(&state)?;
            let msg = format!("Mouse moved to ({x}, {y})");
            out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
            cdp.close().await;
        }
        "down" => {
            let button = args.get(1).map(String::as_str).unwrap_or("left");
            if !matches!(button, "left" | "right" | "middle") {
                bail!("Invalid button: {button}. Use: left, right, middle");
            }
            let state = ctx.load_session_state()?;
            let x = state.mouse_x.unwrap_or(0.0);
            let y = state.mouse_y.unwrap_or(0.0);
            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;
            cdp.send(
                "Input.dispatchMouseEvent",
                json!({ "type": "mousePressed", "x": x, "y": y, "button": button, "clickCount": 1 }),
            )
            .await?;
            let msg = format!("Mouse {button} down at ({x}, {y})");
            out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
            cdp.close().await;
        }
        "up" => {
            let button = args.get(1).map(String::as_str).unwrap_or("left");
            if !matches!(button, "left" | "right" | "middle") {
                bail!("Invalid button: {button}. Use: left, right, middle");
            }
            let state = ctx.load_session_state()?;
            let x = state.mouse_x.unwrap_or(0.0);
            let y = state.mouse_y.unwrap_or(0.0);
            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;
            cdp.send(
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseReleased", "x": x, "y": y, "button": button, "clickCount": 1 }),
            )
            .await?;
            let msg = format!("Mouse {button} up at ({x}, {y})");
            out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
            cdp.close().await;
        }
        "wheel" => {
            let delta_y: f64 = args
                .get(1)
                .and_then(|v| v.parse().ok())
                .context("Usage: mouse wheel <deltaY> [deltaX]")?;
            let delta_x: f64 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(0.0);
            let state = ctx.load_session_state()?;
            let x = state.mouse_x.unwrap_or(0.0);
            let y = state.mouse_y.unwrap_or(0.0);
            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;
            cdp.send(
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": delta_x, "deltaY": delta_y }),
            )
            .await?;
            let msg = format!("Mouse wheel deltaY={delta_y} deltaX={delta_x} at ({x}, {y})");
            out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
            cdp.close().await;
        }
        _ => bail!(
            "Usage: mouse <move|down|up|wheel> [args]\n  mouse move <x> <y>\n  mouse down [left|right|middle]\n  mouse up [left|right|middle]\n  mouse wheel <deltaY> [deltaX]"
        ),
    }
    Ok(())
}
