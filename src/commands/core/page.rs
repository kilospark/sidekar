use super::*;
use crate::output::PlainOutput;

mod capture;
pub(crate) use capture::*;

#[derive(serde::Serialize)]
struct AxInteractiveOutput {
    elements: Vec<crate::types::InteractiveElement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<AxDiffOutput>,
    truncated: bool,
    text: String,
}

#[derive(serde::Serialize)]
struct AxDiffOutput {
    added: Vec<crate::types::InteractiveElement>,
    removed: Vec<crate::types::InteractiveElement>,
    changed: Vec<AxChangedPair>,
    had_previous: bool,
}

#[derive(serde::Serialize)]
struct AxChangedPair {
    from: crate::types::InteractiveElement,
    to: crate::types::InteractiveElement,
}

impl crate::output::CommandOutput for AxInteractiveOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "{}", self.text)
    }
}

#[derive(serde::Serialize)]
struct TextLinesOutput {
    lines: Vec<String>,
    truncated: bool,
}

impl crate::output::CommandOutput for TextLinesOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        for line in &self.lines {
            writeln!(w, "{line}")?;
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct ClickOutput {
    tag: String,
    text: String,
    adopted_tabs: Vec<String>,
    switched_to: Option<String>,
    page_brief: String,
}

impl crate::output::CommandOutput for ClickOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "Clicked {} \"{}\"", self.tag, self.text)?;
        if !self.adopted_tabs.is_empty() {
            writeln!(
                w,
                "Adopted {} new tab(s); switched to [{}]",
                self.adopted_tabs.len(),
                self.switched_to.as_deref().unwrap_or("unknown")
            )?;
        }
        if !self.page_brief.is_empty() {
            writeln!(w, "{}", self.page_brief)?;
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct PressOutput {
    key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    page_brief: Option<String>,
}

impl crate::output::CommandOutput for PressOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "OK press {}", self.key)?;
        if let Some(brief) = &self.page_brief {
            writeln!(w, "{brief}")?;
        }
        Ok(())
    }
}

pub(crate) async fn cmd_navigate(ctx: &mut AppContext, url: &str, dismiss: bool) -> Result<()> {
    let target_url = if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("https://{url}")
    };

    let mut state = ctx.load_session_state()?;
    state.ref_map = None;
    state.ref_map_url = None;
    state.ref_map_timestamp = None;
    ctx.save_session_state(&state)?;

    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    cdp.send("Page.enable", json!({})).await?;
    runtime_evaluate(
        &mut cdp,
        &format!(
            "window.location.href = {}",
            serde_json::to_string(&target_url)?
        ),
        false,
        false,
    )
    .await?;
    wait_for_ready_state_complete(&mut cdp, Duration::from_secs(15)).await?;

    if dismiss {
        sleep(Duration::from_millis(300)).await;
        let _ = runtime_evaluate(&mut cdp, DISMISS_POPUPS_SCRIPT, true, false).await;
        sleep(Duration::from_millis(200)).await;
    }

    let brief = get_page_brief(&mut cdp).await?;
    out!(
        ctx,
        "{}",
        crate::output::to_string(&PlainOutput::new(brief))?
    );
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_dom(
    ctx: &mut AppContext,
    selector: Option<&str>,
    max_tokens: usize,
) -> Result<()> {
    let script = build_dom_extract_script(selector)?;
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let context_id = get_frame_context_id(ctx, &mut cdp).await?;
    let result = runtime_evaluate_with_context(&mut cdp, &script, true, false, context_id).await?;
    let mut dom_output = result
        .pointer("/result/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    if dom_output.starts_with("ERROR: Element not found") {
        let suggest_script = r#"(function() {
            const s = [];
            document.querySelectorAll('[id]').forEach(el => {
                if (s.length < 15) s.push('#' + CSS.escape(el.id));
            });
            document.querySelectorAll('[data-testid]').forEach(el => {
                if (s.length < 20) s.push('[data-testid="' + el.getAttribute('data-testid') + '"]');
            });
            ['main','article','section','nav','header','footer','aside','form','table'].forEach(tag => {
                if (document.querySelector(tag)) s.push(tag);
            });
            document.querySelectorAll('[role]').forEach(el => {
                const r = el.getAttribute('role');
                const sel = '[role="' + r + '"]';
                if (s.length < 30 && !s.includes(sel)) s.push(sel);
            });
            document.querySelectorAll('[aria-label]').forEach(el => {
                if (s.length < 35) s.push('[aria-label="' + el.getAttribute('aria-label').replace(/"/g, '\\"') + '"]');
            });
            if (s.length === 0) {
                const top = document.body.children;
                for (let i = 0; i < Math.min(top.length, 5); i++) {
                    const el = top[i];
                    const tag = el.tagName.toLowerCase();
                    const cls = el.className && typeof el.className === 'string' ? '.' + el.className.trim().split(/\s+/).slice(0,2).join('.') : '';
                    s.push(tag + cls);
                }
            }
            return s;
        })()"#;
        let suggest_result =
            runtime_evaluate_with_context(&mut cdp, suggest_script, true, false, context_id)
                .await?;
        let suggestions = suggest_result
            .pointer("/result/value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !suggestions.is_empty() {
            let sel_list: Vec<&str> = suggestions.iter().filter_map(Value::as_str).collect();
            dom_output = format!(
                "{dom_output}\n\nAvailable selectors: {}",
                sel_list.join(", ")
            );
        }
        out!(
            ctx,
            "{}",
            crate::output::to_string(&PlainOutput::new(dom_output))?
        );
        cdp.close().await;
        return Ok(());
    }

    if dom_output.is_empty() {
        if let Some(sel) = selector {
            let msg = format!("Element matched but has no visible DOM content: {sel}");
            out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
        }
        cdp.close().await;
        return Ok(());
    }

    if max_tokens > 0 {
        let char_budget = max_tokens.saturating_mul(4);
        if dom_output.len() > char_budget {
            let boundary = dom_output.floor_char_boundary(char_budget);
            dom_output = format!(
                "{}\n... (truncated to ~{} tokens)",
                &dom_output[..boundary],
                max_tokens
            );
        }
    }

    out!(
        ctx,
        "{}",
        crate::output::to_string(&PlainOutput::new(dom_output))?
    );
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_axtree_interactive(
    ctx: &mut AppContext,
    max_tokens: usize,
    show_diff: bool,
) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;

    let data = fetch_interactive_elements(ctx, &mut cdp).await?;
    if show_diff {
        let state = ctx.load_session_state()?;
        let (diff_out, text) = if let (Some(prev), Some(curr)) =
            (state.prev_elements, state.current_elements)
        {
            let (added, removed, changed) = diff_elements(&prev, &curr);
            let text = if added.is_empty() && removed.is_empty() && changed.is_empty() {
                "(no changes since last snapshot)".to_string()
            } else {
                let mut diff_buf = String::new();
                if !added.is_empty() {
                    diff_buf.push_str("ADDED:\n");
                    for e in &added {
                        diff_buf
                            .push_str(&format!("  + [{}] {} \"{}\"\n", e.ref_id, e.role, e.name));
                    }
                }
                if !removed.is_empty() {
                    diff_buf.push_str("REMOVED:\n");
                    for e in &removed {
                        diff_buf
                            .push_str(&format!("  - [{}] {} \"{}\"\n", e.ref_id, e.role, e.name));
                    }
                }
                if !changed.is_empty() {
                    diff_buf.push_str("CHANGED:\n");
                    for (from, to) in &changed {
                        diff_buf.push_str(&format!(
                            "  ~ [{}] {} \"{}\" (was: \"{}\")\n",
                            to.ref_id, to.role, to.name, from.name
                        ));
                    }
                }
                diff_buf.push_str(&format!(
                    "({} added, {} removed, {} changed)",
                    added.len(),
                    removed.len(),
                    changed.len()
                ));
                diff_buf.trim_end().to_string()
            };
            (
                Some(AxDiffOutput {
                    added,
                    removed,
                    changed: changed
                        .into_iter()
                        .map(|(from, to)| AxChangedPair { from, to })
                        .collect(),
                    had_previous: true,
                }),
                text,
            )
        } else {
            (
                Some(AxDiffOutput {
                    added: Vec::new(),
                    removed: Vec::new(),
                    changed: Vec::new(),
                    had_previous: false,
                }),
                format!("(no previous snapshot to diff against)\n{}", data.output),
            )
        };
        let output = AxInteractiveOutput {
            elements: data.elements,
            diff: diff_out,
            truncated: false,
            text,
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
        cdp.close().await;
        return Ok(());
    }

    let mut axtree_output = data.output;
    let mut truncated = false;
    if max_tokens > 0 {
        let char_budget = max_tokens.saturating_mul(4);
        if axtree_output.len() > char_budget {
            let boundary = axtree_output.floor_char_boundary(char_budget);
            axtree_output = format!(
                "{}\n... (truncated to ~{} tokens)",
                &axtree_output[..boundary],
                max_tokens
            );
            truncated = true;
        }
    }
    let output = AxInteractiveOutput {
        elements: data.elements,
        diff: None,
        truncated,
        text: axtree_output,
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_axtree_full(
    ctx: &mut AppContext,
    selector: Option<&str>,
    max_tokens: usize,
) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    cdp.send("Accessibility.enable", json!({})).await?;

    let mut output = if let Some(sel) = selector {
        let context_id = get_frame_context_id(ctx, &mut cdp).await?;
        let obj_result = runtime_evaluate_with_context(
            &mut cdp,
            &format!("document.querySelector({})", serde_json::to_string(sel)?),
            false,
            false,
            context_id,
        )
        .await?;
        let object_id = obj_result
            .pointer("/result/objectId")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("Element not found: {sel}"))?;
        let result = cdp
            .send(
                "Accessibility.queryAXTree",
                json!({ "objectId": object_id }),
            )
            .await?;
        serde_json::to_string_pretty(&result)?
    } else {
        let result = cdp.send("Accessibility.getFullAXTree", json!({})).await?;
        serde_json::to_string_pretty(&result)?
    };

    if max_tokens > 0 {
        let char_budget = max_tokens.saturating_mul(4);
        if output.len() > char_budget {
            let boundary = output.floor_char_boundary(char_budget);
            output = format!(
                "{}\n... (truncated to ~{} tokens — use ax-tree -i for interactive elements or ax-tree with selector to scope)",
                &output[..boundary],
                max_tokens
            );
        }
    }

    out!(
        ctx,
        "{}",
        crate::output::to_string(&PlainOutput::new(output))?
    );
    cdp.send("Accessibility.disable", json!({})).await?;
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_read(
    ctx: &mut AppContext,
    selector: Option<&str>,
    max_tokens: usize,
) -> Result<()> {
    let script = build_read_extract_script(selector)?;
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    wait_for_network_idle(&mut cdp, 800, 5000).await?;

    let context_id = get_frame_context_id(ctx, &mut cdp).await?;
    let result = runtime_evaluate_with_context(&mut cdp, &script, true, false, context_id).await?;
    let mut output = result
        .pointer("/result/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    if output.len() < 200 && selector.is_none() {
        wait_for_network_idle(&mut cdp, 1000, 5000).await?;
        let retry =
            runtime_evaluate_with_context(&mut cdp, &script, true, false, context_id).await?;
        let retry_text = retry
            .pointer("/result/value")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if retry_text.len() > output.len() {
            output = retry_text.to_string();
        }
    }

    if output.len() < 100 && selector.is_none() {
        let fallback = runtime_evaluate_with_context(
            &mut cdp,
            "document.body?.innerText?.substring(0, 50000) || ''",
            true,
            false,
            context_id,
        )
        .await?;
        let fallback_text = fallback
            .pointer("/result/value")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if fallback_text.len() > output.len() {
            output = fallback_text.to_string();
        }
    }

    if max_tokens > 0 {
        let char_budget = max_tokens.saturating_mul(4);
        if output.len() > char_budget {
            let boundary = output.floor_char_boundary(char_budget);
            output = format!(
                "{}\n... (truncated to ~{} tokens)",
                &output[..boundary],
                max_tokens
            );
        }
    }

    out!(
        ctx,
        "{}",
        crate::output::to_string(&PlainOutput::new(output))?
    );
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_text(
    ctx: &mut AppContext,
    selector: Option<&str>,
    max_tokens: usize,
) -> Result<()> {
    let script = build_text_extract_script(selector)?;
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let context_id = get_frame_context_id(ctx, &mut cdp).await?;
    let result = runtime_evaluate_with_context(&mut cdp, &script, true, false, context_id).await?;
    let raw = result
        .pointer("/result/value")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let parsed: Value = serde_json::from_str(raw).unwrap_or(Value::Null);

    if let Some(err) = parsed.get("error").and_then(Value::as_str) {
        let msg = format!("ERROR: {err}");
        out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
        cdp.close().await;
        return Ok(());
    }

    let lines_val = parsed
        .get("lines")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let ref_map_val = parsed.get("refMap").cloned().unwrap_or(json!({}));

    if let Some(obj) = ref_map_val.as_object() {
        let ref_map: HashMap<String, String> = obj
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect();
        if !ref_map.is_empty() {
            let mut state = ctx.load_session_state()?;
            state.ref_map = Some(ref_map);
            let url_result = runtime_evaluate(&mut cdp, "location.href", true, false).await?;
            state.ref_map_url = url_result
                .pointer("/result/value")
                .and_then(Value::as_str)
                .map(|s| s.to_string());
            state.ref_map_timestamp = Some(now_epoch_ms());
            ctx.save_session_state(&state)?;
        }
    }

    let mut lines: Vec<String> = lines_val
        .iter()
        .filter_map(Value::as_str)
        .map(|s| s.to_string())
        .collect();
    let mut truncated = false;

    if max_tokens > 0 {
        let char_budget = max_tokens.saturating_mul(4);
        let joined_len: usize = lines.iter().map(|l| l.len() + 1).sum();
        if joined_len > char_budget {
            let mut acc = 0usize;
            let mut cut_idx = lines.len();
            for (i, line) in lines.iter().enumerate() {
                acc = acc.saturating_add(line.len() + 1);
                if acc > char_budget {
                    cut_idx = i;
                    break;
                }
            }
            lines.truncate(cut_idx);
            lines.push(format!("... (truncated to ~{} tokens)", max_tokens));
            truncated = true;
        }
    }

    let output = TextLinesOutput { lines, truncated };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_click(ctx: &mut AppContext, selector: &str) -> Result<()> {
    let tabs_before = snapshot_tab_ids(ctx).await?;
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let loc = locate_element(ctx, &mut cdp, selector).await?;

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

    let click_tag = loc.tag.to_lowercase();
    let click_text = loc.text.clone();

    let _ = cdp.send("Network.enable", json!({})).await;
    sleep(Duration::from_millis(150)).await;

    let adopted = adopt_new_tabs(ctx, &tabs_before, Duration::from_millis(800)).await?;
    if !adopted.is_empty() {
        let _ = cdp.send("Network.disable", json!({})).await;
        cdp.close().await;
        let mut adopted_cdp = open_cdp(ctx).await?;
        prepare_cdp(ctx, &mut adopted_cdp).await?;
        let switched_to = adopted
            .iter()
            .find(|tab| tab.url.as_deref().is_some_and(|url| url != "about:blank"))
            .or_else(|| adopted.first())
            .map(|tab| tab.id.clone());
        let brief = get_page_brief(&mut adopted_cdp).await?;
        adopted_cdp.close().await;
        let output = ClickOutput {
            tag: click_tag,
            text: click_text,
            adopted_tabs: adopted.iter().map(|t| t.id.clone()).collect(),
            switched_to,
            page_brief: brief,
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
        return Ok(());
    }

    let mut inflight: i32 = 0;
    let mut had_network = false;

    loop {
        let Some(event) = cdp.next_event(Duration::from_millis(0)).await? else {
            break;
        };
        if event.is_null() {
            break;
        }
        match event
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "Network.requestWillBeSent" => {
                inflight += 1;
                had_network = true;
            }
            "Network.loadingFinished" | "Network.loadingFailed" => {
                inflight -= 1;
            }
            _ => {}
        }
    }

    if had_network && inflight > 0 {
        let net_deadline = Instant::now() + Duration::from_millis(3000);
        let quiet_for = Duration::from_millis(200);
        let mut last_activity = Instant::now();

        while Instant::now() < net_deadline {
            let remain = std::cmp::min(
                net_deadline.saturating_duration_since(Instant::now()),
                Duration::from_millis(50),
            );
            let Some(event) = cdp.next_event(remain).await? else {
                if inflight <= 0 && Instant::now().duration_since(last_activity) >= quiet_for {
                    break;
                }
                continue;
            };
            if event.is_null() {
                if inflight <= 0 && Instant::now().duration_since(last_activity) >= quiet_for {
                    break;
                }
                continue;
            }
            match event
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "Network.requestWillBeSent" => {
                    inflight += 1;
                    last_activity = Instant::now();
                }
                "Network.loadingFinished" | "Network.loadingFailed" => {
                    inflight -= 1;
                    last_activity = Instant::now();
                }
                _ => {}
            }
        }

        let _ = runtime_evaluate(
            &mut cdp,
            "new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))",
            true,
            true,
        )
        .await;
    }

    let _ = cdp.send("Network.disable", json!({})).await;
    let brief = get_page_brief(&mut cdp).await?;
    cdp.close().await;
    let output = ClickOutput {
        tag: click_tag,
        text: click_text,
        adopted_tabs: Vec::new(),
        switched_to: None,
        page_brief: brief,
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

pub(crate) async fn cmd_type(ctx: &mut AppContext, selector: &str, text: &str) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let context_id = get_frame_context_id(ctx, &mut cdp).await?;
    type_text_verified(&mut cdp, context_id, selector, text).await?;

    let msg = format!("Typed \"{}\" into {selector}", truncate(text, 50));
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_press(ctx: &mut AppContext, key: &str) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;

    if key.contains('+') {
        let (mods, main_key) = parse_key_combo(key);
        let mapping = key_mapping(&main_key);
        let mod_bits = (if mods.alt { 1 } else { 0 })
            | (if mods.ctrl { 2 } else { 0 })
            | (if mods.meta { 4 } else { 0 })
            | (if mods.shift { 8 } else { 0 });

        cdp.send(
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyDown",
                "key": mapping.key,
                "code": mapping.code,
                "keyCode": mapping.key_code,
                "windowsVirtualKeyCode": mapping.key_code,
                "modifiers": mod_bits,
            }),
        )
        .await?;

        cdp.send(
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyUp",
                "key": mapping.key,
                "code": mapping.code,
                "keyCode": mapping.key_code,
                "windowsVirtualKeyCode": mapping.key_code,
                "modifiers": mod_bits,
            }),
        )
        .await?;

        let brief = if matches!(main_key.to_lowercase().as_str(), "enter" | "tab" | "escape") {
            sleep(Duration::from_millis(150)).await;
            Some(get_page_brief(&mut cdp).await?)
        } else {
            None
        };
        let output = PressOutput {
            key: key.to_string(),
            page_brief: brief,
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
        cdp.close().await;
        return Ok(());
    }

    let mapping = key_mapping(key);
    cdp.send(
        "Input.dispatchKeyEvent",
        json!({
            "type": "keyDown",
            "key": mapping.key,
            "code": mapping.code,
            "keyCode": mapping.key_code,
            "windowsVirtualKeyCode": mapping.key_code,
        }),
    )
    .await?;
    cdp.send(
        "Input.dispatchKeyEvent",
        json!({
            "type": "keyUp",
            "key": mapping.key,
            "code": mapping.code,
            "keyCode": mapping.key_code,
            "windowsVirtualKeyCode": mapping.key_code,
        }),
    )
    .await?;

    let brief = if matches!(key.to_lowercase().as_str(), "enter" | "tab" | "escape") {
        sleep(Duration::from_millis(150)).await;
        Some(get_page_brief(&mut cdp).await?)
    } else {
        None
    };
    let output = PressOutput {
        key: key.to_string(),
        page_brief: brief,
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    cdp.close().await;
    Ok(())
}
