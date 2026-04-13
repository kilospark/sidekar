use super::*;
use crate::output::PlainOutput;

#[derive(serde::Serialize)]
struct ScreenshotOutput {
    path: String,
    size_kb: u64,
    est_vision_tokens: u64,
    annotated: usize,
    tip: Option<String>,
}

impl crate::output::CommandOutput for ScreenshotOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "Screenshot saved to {}", self.path)?;
        writeln!(
            w,
            "Size: {}KB | Est. vision tokens: ~{}",
            self.size_kb, self.est_vision_tokens
        )?;
        if self.annotated > 0 {
            writeln!(w, "Annotated {} interactive elements", self.annotated)?;
        }
        if let Some(t) = &self.tip {
            writeln!(w, "{t}")?;
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct GridOutput {
    action: String,
    cols: u64,
    rows: u64,
}

impl crate::output::CommandOutput for GridOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.action == "off" {
            writeln!(w, "Grid overlay removed.")?;
        } else if self.cols > 0 && self.rows > 0 {
            writeln!(
                w,
                "Grid overlay: {}x{}. Each cell shows its center coordinate. Use 'grid off' to remove.",
                self.cols, self.rows
            )?;
        } else {
            writeln!(w, "Grid overlay applied.")?;
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct ScreencastOutput {
    action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    frame_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frame_size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl crate::output::CommandOutput for ScreencastOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if let Some(msg) = &self.message {
            writeln!(w, "{msg}")?;
        }
        Ok(())
    }
}

pub(crate) async fn cmd_screenshot(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let mut selector: Option<String> = None;
    let mut format = "jpeg".to_string();
    let mut quality: u32 = 80;
    let mut ref_id: Option<String> = None;
    let mut pad: u32 = 48;
    let mut full_page = false;
    let mut output_path: Option<String> = None;
    let mut scale_factor: Option<f64> = None;
    let mut annotate = false;

    for arg in args {
        if let Some(v) = arg.strip_prefix("--output=") {
            output_path = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--selector=") {
            selector = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--format=") {
            format = if v == "png" {
                "png".to_string()
            } else {
                "jpeg".to_string()
            };
        } else if let Some(v) = arg.strip_prefix("--quality=") {
            quality = v.parse().unwrap_or(80).clamp(1, 100);
        } else if let Some(v) = arg.strip_prefix("--ref=") {
            ref_id = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--pad=") {
            pad = v.parse().unwrap_or(48);
        } else if arg == "--full" {
            full_page = true;
        } else if arg == "--annotate" {
            annotate = true;
        } else if let Some(v) = arg.strip_prefix("--scale=") {
            scale_factor = v.parse().ok();
        }
    }

    if let Some(rid) = &ref_id {
        selector = Some(resolve_selector(ctx, rid)?);
    }

    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;

    let mut params = json!({ "format": &format });

    if format == "jpeg" {
        params["quality"] = json!(quality);
    }

    if let Some(sel) = &selector {
        let context_id = get_frame_context_id(ctx, &mut cdp).await?;
        let pad_val = if ref_id.is_some() { pad } else { 0 };
        let script = format!(
            r#"(function() {{
                const el = document.querySelector({sel});
                if (!el) return {{ error: 'Element not found: ' + {sel} }};
                el.scrollIntoView({{ block: 'center', inline: 'center', behavior: 'instant' }});
                const rect = el.getBoundingClientRect();
                const pad = {pad};
                const x = Math.max(0, rect.x - pad);
                const y = Math.max(0, rect.y - pad);
                const w = rect.width + pad * 2;
                const h = rect.height + pad * 2;
                return {{ x: x, y: y, width: w, height: h }};
            }})()"#,
            sel = serde_json::to_string(sel.as_str())?,
            pad = pad_val
        );
        let result =
            runtime_evaluate_with_context(&mut cdp, &script, true, false, context_id).await?;
        let value = result
            .pointer("/result/value")
            .cloned()
            .unwrap_or(Value::Null);
        if let Some(err) = value.get("error").and_then(Value::as_str) {
            bail!("{err}");
        }
        let x = value.get("x").and_then(Value::as_f64).unwrap_or(0.0);
        let y = value.get("y").and_then(Value::as_f64).unwrap_or(0.0);
        let w = value.get("width").and_then(Value::as_f64).unwrap_or(0.0);
        let h = value.get("height").and_then(Value::as_f64).unwrap_or(0.0);
        if w > 0.0 && h > 0.0 {
            params["clip"] = json!({
                "x": x, "y": y, "width": w, "height": h, "scale": 1
            });
        }
    }

    let vp_result = runtime_evaluate(
        &mut cdp,
        "[window.innerWidth, window.innerHeight, window.devicePixelRatio, Math.max(document.body.scrollHeight, document.documentElement.scrollHeight)]",
        true,
        false,
    )
    .await?;
    let vp_arr = vp_result.pointer("/result/value").and_then(Value::as_array);
    let viewport_w = vp_arr
        .and_then(|a| a.first())
        .and_then(Value::as_f64)
        .unwrap_or(1280.0);
    let viewport_h = vp_arr
        .and_then(|a| a.get(1))
        .and_then(Value::as_f64)
        .unwrap_or(800.0);
    let dpr = vp_arr
        .and_then(|a| a.get(2))
        .and_then(Value::as_f64)
        .unwrap_or(1.0);
    let doc_height = vp_arr
        .and_then(|a| a.get(3))
        .and_then(Value::as_f64)
        .unwrap_or(viewport_h);

    let capture_h = if full_page { doc_height } else { viewport_h };
    let has_clip = params.get("clip").is_some();
    let effective_scale = if let Some(sf) = scale_factor {
        Some(sf / dpr)
    } else if has_clip && ref_id.is_some() {
        if dpr > 1.0 { Some(1.0 / dpr) } else { None }
    } else {
        Some(800.0 / (viewport_w * dpr))
    };

    if let Some(scale) = effective_scale {
        if let Some(clip) = params.get_mut("clip") {
            clip["scale"] = json!(scale);
        } else {
            params["clip"] = json!({
                "x": 0, "y": 0,
                "width": viewport_w,
                "height": capture_h,
                "scale": scale
            });
        }
    } else if full_page && !has_clip {
        params["clip"] = json!({
            "x": 0, "y": 0,
            "width": viewport_w,
            "height": capture_h,
            "scale": 1
        });
    }

    if full_page {
        params["captureBeyondViewport"] = json!(true);
    }

    let mut annotation_count = 0usize;
    if annotate {
        let data = fetch_interactive_elements(ctx, &mut cdp).await?;
        if !data.elements.is_empty() {
            let state = ctx.load_session_state()?;
            let ref_map = state.ref_map.clone().unwrap_or_default();
            let selectors_json: Vec<Value> = data
                .elements
                .iter()
                .filter_map(|el| {
                    ref_map
                        .get(&el.ref_id.to_string())
                        .map(|sel| json!({"ref": el.ref_id, "sel": sel}))
                })
                .collect();
            annotation_count = selectors_json.len();
            let inject_js = format!(
                r#"(() => {{
                    const items = {items};
                    const overlay = document.createElement('div');
                    overlay.id = 'sidekar-annotations';
                    overlay.style.cssText = 'position:fixed;inset:0;pointer-events:none;z-index:2147483647;overflow:visible';
                    for (const item of items) {{
                        const el = document.querySelector(item.sel);
                        if (!el) continue;
                        const rect = el.getBoundingClientRect();
                        if (rect.width === 0 && rect.height === 0) continue;
                        const border = document.createElement('div');
                        border.style.cssText = `position:fixed;left:${{rect.x}}px;top:${{rect.y}}px;width:${{rect.width}}px;height:${{rect.height}}px;border:2px solid rgba(255,0,0,0.8);border-radius:2px;box-sizing:border-box`;
                        const label = document.createElement('div');
                        const above = rect.y > 16;
                        label.style.cssText = `position:fixed;left:${{rect.x}}px;top:${{above ? rect.y - 16 : rect.y + rect.height + 1}}px;background:rgba(255,0,0,0.85);color:#fff;font:bold 11px/14px monospace;padding:0 3px;border-radius:1px;white-space:nowrap`;
                        label.textContent = item.ref;
                        overlay.appendChild(border);
                        overlay.appendChild(label);
                    }}
                    document.body.appendChild(overlay);
                    return items.length;
                }})()"#,
                items = serde_json::to_string(&selectors_json)?
            );
            let context_id = get_frame_context_id(ctx, &mut cdp).await?;
            runtime_evaluate_with_context(&mut cdp, &inject_js, true, false, context_id).await?;
        }
    }

    let result = cdp.send("Page.captureScreenshot", params.clone()).await?;

    if annotate && annotation_count > 0 {
        let context_id = get_frame_context_id(ctx, &mut cdp).await?;
        let _ = runtime_evaluate_with_context(
            &mut cdp,
            "document.getElementById('sidekar-annotations')?.remove()",
            true,
            false,
            context_id,
        )
        .await;
    }

    let data = result
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Missing screenshot data"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .context("Failed to decode screenshot data")?;

    let ext = if format == "png" { "png" } else { "jpeg" };
    let out = if let Some(ref p) = output_path {
        PathBuf::from(p)
    } else {
        let sid = ctx
            .current_session_id
            .clone()
            .unwrap_or_else(|| "default".to_string());
        ctx.tmp_dir()
            .join(format!("sidekar-screenshot-{sid}.{ext}"))
    };

    fs::write(&out, &bytes).with_context(|| format!("failed writing {}", out.display()))?;

    let file_kb = bytes.len() / 1024;
    let est_tokens = if let Some(clip) = params.get("clip") {
        let cw = clip
            .get("width")
            .and_then(Value::as_f64)
            .unwrap_or(viewport_w);
        let ch = clip
            .get("height")
            .and_then(Value::as_f64)
            .unwrap_or(viewport_h);
        let cs = clip.get("scale").and_then(Value::as_f64).unwrap_or(1.0);
        let pw = (cw * cs) as u64;
        let ph = (ch * cs) as u64;
        (pw * ph) / 750
    } else {
        let pw = (viewport_w / dpr) as u64;
        let ph = (viewport_h / dpr) as u64;
        (pw * ph) / 750
    };
    let tip = if est_tokens > 500 && ref_id.is_none() && selector.is_none() {
        Some("Tip: Use ref=N, selector, or --width to reduce cost.".to_string())
    } else {
        None
    };
    let output = ScreenshotOutput {
        path: out.display().to_string(),
        size_kb: file_kb as u64,
        est_vision_tokens: est_tokens,
        annotated: if annotate { annotation_count } else { 0 },
        tip,
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_pdf(ctx: &mut AppContext, output_path: Option<&str>) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let result = cdp
        .send(
            "Page.printToPDF",
            json!({
                "printBackground": true,
                "preferCSSPageSize": true
            }),
        )
        .await?;
    let data = result
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Missing PDF data"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .context("Failed to decode PDF data")?;
    let sid = ctx
        .current_session_id
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let out = output_path
        .map(PathBuf::from)
        .unwrap_or_else(|| ctx.tmp_dir().join(format!("sidekar-page-{sid}.pdf")));
    fs::write(&out, bytes).with_context(|| format!("failed writing {}", out.display()))?;
    let msg = format!("PDF saved to {}", out.display());
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_grid(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let context_id = get_frame_context_id(ctx, &mut cdp).await?;

    let first = args.first().map(String::as_str).unwrap_or("");

    if first == "off" {
        runtime_evaluate_with_context(
            &mut cdp,
            "document.getElementById('sidekar-grid-overlay')?.remove()",
            false,
            false,
            context_id,
        )
        .await?;
        let output = GridOutput {
            action: "off".to_string(),
            cols: 0,
            rows: 0,
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
        return Ok(());
    }

    let (cols, rows) = if first.is_empty() {
        (10, 10)
    } else if first.contains('x') {
        let parts: Vec<&str> = first.split('x').collect();
        let c = parts[0].parse::<u32>().unwrap_or(10);
        let r = parts
            .get(1)
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(10);
        (c, r)
    } else if let Ok(px) = first.parse::<u32>() {
        (px, 0)
    } else {
        (10, 10)
    };

    let script = if rows == 0 {
        format!(
            r#"(function() {{
            document.getElementById('sidekar-grid-overlay')?.remove();
            const px = {cols};
            const vw = window.innerWidth, vh = window.innerHeight;
            const c = Math.ceil(vw / px), r = Math.ceil(vh / px);
            const d = document.createElement('div');
            d.id = 'sidekar-grid-overlay';
            d.style.cssText = 'position:fixed;top:0;left:0;width:100%;height:100%;z-index:999999;pointer-events:none;display:grid;grid-template-columns:repeat('+c+',1fr);grid-template-rows:repeat('+r+',1fr)';
            for (let row = 0; row < r; row++) {{
                for (let col = 0; col < c; col++) {{
                    const cell = document.createElement('div');
                    const cx = Math.round(col * px + px/2);
                    const cy = Math.round(row * px + px/2);
                    cell.style.cssText = 'border:1px solid rgba(255,0,0,0.3);display:flex;align-items:center;justify-content:center;font:9px monospace;color:rgba(255,0,0,0.7);background:rgba(255,255,255,0.05)';
                    cell.textContent = cx+','+cy;
                    d.appendChild(cell);
                }}
            }}
            document.body.appendChild(d);
            return {{ cols: c, rows: r, cellPx: px }};
        }})()"#
        )
    } else {
        format!(
            r#"(function() {{
            document.getElementById('sidekar-grid-overlay')?.remove();
            const cols = {cols}, rows = {rows};
            const vw = window.innerWidth, vh = window.innerHeight;
            const cw = vw / cols, ch = vh / rows;
            const d = document.createElement('div');
            d.id = 'sidekar-grid-overlay';
            d.style.cssText = 'position:fixed;top:0;left:0;width:100%;height:100%;z-index:999999;pointer-events:none;display:grid;grid-template-columns:repeat('+cols+',1fr);grid-template-rows:repeat('+rows+',1fr)';
            for (let row = 0; row < rows; row++) {{
                for (let col = 0; col < cols; col++) {{
                    const cell = document.createElement('div');
                    const cx = Math.round(col * cw + cw/2);
                    const cy = Math.round(row * ch + ch/2);
                    cell.style.cssText = 'border:1px solid rgba(255,0,0,0.3);display:flex;align-items:center;justify-content:center;font:9px monospace;color:rgba(255,0,0,0.7);background:rgba(255,255,255,0.05)';
                    cell.textContent = cx+','+cy;
                    d.appendChild(cell);
                }}
            }}
            document.body.appendChild(d);
            return {{ cols, rows, cellW: Math.round(cw), cellH: Math.round(ch) }};
        }})()"#
        )
    };

    let result = runtime_evaluate_with_context(&mut cdp, &script, true, false, context_id).await?;

    let output = if let Some(val) = result.pointer("/result/value") {
        GridOutput {
            action: "on".to_string(),
            cols: val.get("cols").and_then(Value::as_u64).unwrap_or(0),
            rows: val.get("rows").and_then(Value::as_u64).unwrap_or(0),
        }
    } else {
        GridOutput {
            action: "on".to_string(),
            cols: 0,
            rows: 0,
        }
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

pub(crate) async fn cmd_screencast(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("");
    match action {
        "start" => {
            let quality: u32 = args
                .iter()
                .find_map(|a| a.strip_prefix("--quality="))
                .and_then(|v| v.parse().ok())
                .unwrap_or(50);
            let every_nth: u32 = args
                .iter()
                .find_map(|a| a.strip_prefix("--fps="))
                .and_then(|v| v.parse::<u32>().ok())
                .map(|fps| if fps > 0 { 30 / fps.min(30) } else { 15 })
                .unwrap_or(15);
            let max_width: u32 = args
                .iter()
                .find_map(|a| a.strip_prefix("--width="))
                .and_then(|v| v.parse().ok())
                .unwrap_or(1280);
            let max_height: u32 = args
                .iter()
                .find_map(|a| a.strip_prefix("--height="))
                .and_then(|v| v.parse().ok())
                .unwrap_or(800);

            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;
            cdp.send(
                "Page.startScreencast",
                json!({
                    "format": "jpeg",
                    "quality": quality,
                    "maxWidth": max_width,
                    "maxHeight": max_height,
                    "everyNthFrame": every_nth,
                }),
            )
            .await?;

            let sid = ctx
                .current_session_id
                .clone()
                .unwrap_or_else(|| "default".to_string());
            let frame_path = ctx.tmp_dir().join(format!("sidekar-screencast-{sid}.jpg"));
            let mut frames_received = 0u32;
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let remain = deadline.saturating_duration_since(Instant::now());
                let Some(event) = cdp.next_event(remain).await? else {
                    break;
                };
                if event.get("method").and_then(Value::as_str) == Some("Page.screencastFrame") {
                    if let Some(params) = event.get("params") {
                        let session_id =
                            params.get("sessionId").and_then(Value::as_i64).unwrap_or(0);
                        if let Some(data) = params.get("data").and_then(Value::as_str)
                            && let Ok(bytes) =
                                base64::engine::general_purpose::STANDARD.decode(data)
                        {
                            let _ = fs::write(&frame_path, &bytes);
                            frames_received += 1;
                        }
                        let _ = cdp
                            .send(
                                "Page.screencastFrameAck",
                                json!({ "sessionId": session_id }),
                            )
                            .await;
                    }
                    break;
                }
            }

            let mut state = ctx.load_session_state()?;
            state.screencast_active = Some(true);
            ctx.save_session_state(&state)?;

            let output = if frames_received > 0 {
                ScreencastOutput {
                    action: "start".to_string(),
                    frame_path: Some(frame_path.display().to_string()),
                    frame_size: None,
                    message: Some(format!(
                        "Screencast started. Latest frame: {}",
                        frame_path.display()
                    )),
                }
            } else {
                ScreencastOutput {
                    action: "start".to_string(),
                    frame_path: None,
                    frame_size: None,
                    message: Some("Screencast started (no initial frame captured).".to_string()),
                }
            };
            out!(ctx, "{}", crate::output::to_string(&output)?);
            cdp.close().await;
        }
        "stop" => {
            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;
            cdp.send("Page.stopScreencast", json!({})).await?;
            let mut state = ctx.load_session_state()?;
            state.screencast_active = Some(false);
            ctx.save_session_state(&state)?;
            let output = ScreencastOutput {
                action: "stop".to_string(),
                frame_path: None,
                frame_size: None,
                message: Some("Screencast stopped.".to_string()),
            };
            out!(ctx, "{}", crate::output::to_string(&output)?);
            cdp.close().await;
        }
        "frame" => {
            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;

            let sid = ctx
                .current_session_id
                .clone()
                .unwrap_or_else(|| "default".to_string());
            let frame_path = ctx.tmp_dir().join(format!("sidekar-screencast-{sid}.jpg"));

            let deadline = Instant::now() + Duration::from_secs(3);
            let mut got_frame = false;
            while Instant::now() < deadline {
                let remain = deadline.saturating_duration_since(Instant::now());
                let Some(event) = cdp.next_event(remain).await? else {
                    break;
                };
                if event.get("method").and_then(Value::as_str) == Some("Page.screencastFrame") {
                    if let Some(params) = event.get("params") {
                        let session_id =
                            params.get("sessionId").and_then(Value::as_i64).unwrap_or(0);
                        if let Some(data) = params.get("data").and_then(Value::as_str)
                            && let Ok(bytes) =
                                base64::engine::general_purpose::STANDARD.decode(data)
                        {
                            let _ = fs::write(&frame_path, &bytes);
                            got_frame = true;
                        }
                        let _ = cdp
                            .send(
                                "Page.screencastFrameAck",
                                json!({ "sessionId": session_id }),
                            )
                            .await;
                    }
                    break;
                }
            }

            if got_frame || frame_path.exists() {
                let size = fs::metadata(&frame_path)
                    .map(|m| human_size(m.len()))
                    .unwrap_or_else(|_| "?".to_string());
                let output = ScreencastOutput {
                    action: "frame".to_string(),
                    frame_path: Some(frame_path.display().to_string()),
                    frame_size: Some(size.clone()),
                    message: Some(format!("Frame: {} ({})", frame_path.display(), size)),
                };
                out!(ctx, "{}", crate::output::to_string(&output)?);
            } else {
                bail!("No screencast frame available. Run: screencast start");
            }
            cdp.close().await;
        }
        _ => bail!(
            "Usage: screencast <start|stop|frame> [--fps=N] [--quality=N] [--width=N] [--height=N]"
        ),
    }
    Ok(())
}
