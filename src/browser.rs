use crate::*;

pub fn check_js_error(result: &Value) -> Result<()> {
    if let Some(err) = result
        .pointer("/result/value/error")
        .and_then(Value::as_str)
    {
        bail!("{err}");
    }
    Ok(())
}

pub async fn runtime_evaluate(
    cdp: &mut CdpClient,
    expression: &str,
    return_by_value: bool,
    await_promise: bool,
) -> Result<Value> {
    runtime_evaluate_with_context(cdp, expression, return_by_value, await_promise, None).await
}

pub async fn runtime_evaluate_with_context(
    cdp: &mut CdpClient,
    expression: &str,
    return_by_value: bool,
    await_promise: bool,
    context_id: Option<i64>,
) -> Result<Value> {
    let mut params = json!({ "expression": expression });
    if return_by_value {
        params["returnByValue"] = Value::Bool(true);
    }
    if await_promise {
        params["awaitPromise"] = Value::Bool(true);
    }
    if let Some(id) = context_id {
        params["contextId"] = Value::from(id);
    }

    let result = cdp.send("Runtime.evaluate", params).await?;
    if let Some(details) = result.get("exceptionDetails") {
        let text = details
            .get("text")
            .and_then(Value::as_str)
            .or_else(|| {
                details
                    .get("exception")
                    .and_then(|ex| ex.get("description"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("Runtime evaluation failed");
        bail!("{text}");
    }

    Ok(result)
}

pub async fn get_frame_context_id(ctx: &AppContext, cdp: &mut CdpClient) -> Result<Option<i64>> {
    let state = ctx.load_session_state()?;
    if let Some(frame_id) = state.active_frame_id {
        let result = cdp
            .send(
                "Page.createIsolatedWorld",
                json!({
                    "frameId": frame_id,
                    "worldName": "sidekar",
                    "grantUniversalAccess": true
                }),
            )
            .await?;
        let context_id = result
            .get("executionContextId")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("Could not find execution context for selected frame"))?;
        return Ok(Some(context_id));
    }
    Ok(None)
}

pub async fn prepare_cdp(ctx: &mut AppContext, cdp: &mut CdpClient) -> Result<()> {
    let mut state = ctx.load_session_state()?;

    if let Some(handler) = state.dialog_handler.clone() {
        cdp.send("Page.enable", json!({})).await?;
        cdp.set_auto_dialog(handler.accept, handler.prompt_text);
        state.dialog_handler = None;
        ctx.save_session_state(&state)?;
    }

    if let Some(block_patterns) = state.block_patterns {
        let mut blocked = block_patterns.url_patterns;
        for rt in block_patterns.resource_types {
            blocked.extend(resource_type_url_patterns(&rt));
        }
        if !blocked.is_empty() {
            cdp.send("Network.enable", json!({})).await?;
            let uniq = blocked
                .into_iter()
                .collect::<HashSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            cdp.send("Network.setBlockedURLs", json!({ "urls": uniq }))
                .await?;
        }
    }

    Ok(())
}

pub async fn get_page_brief(cdp: &mut CdpClient) -> Result<String> {
    let result = runtime_evaluate(cdp, PAGE_BRIEF_SCRIPT, true, false).await?;
    Ok(result
        .pointer("/result/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

pub async fn wait_for_ready_state_complete(cdp: &mut CdpClient, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() <= deadline {
        let result = runtime_evaluate(cdp, "document.readyState", true, false).await?;
        let state = result
            .pointer("/result/value")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if state == "complete" {
            return Ok(());
        }
        sleep(Duration::from_millis(300)).await;
    }
    Ok(())
}

pub async fn wait_for_network_idle(
    cdp: &mut CdpClient,
    quiet_ms: u64,
    timeout_ms: u64,
) -> Result<()> {
    cdp.send("Network.enable", json!({})).await?;

    let mut inflight: i32 = 0;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let quiet = Duration::from_millis(quiet_ms);
    let mut last_activity = Instant::now();

    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        if inflight <= 0 && now.duration_since(last_activity) >= quiet {
            break;
        }
        let remain = std::cmp::min(deadline.saturating_duration_since(now), quiet);
        let Some(event) = cdp.next_event(remain).await? else {
            continue;
        };
        if event.is_null() {
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

    cdp.send("Network.disable", json!({})).await?;
    Ok(())
}

pub async fn locate_element(
    ctx: &AppContext,
    cdp: &mut CdpClient,
    selector: &str,
) -> Result<LocatedElement> {
    let context_id = get_frame_context_id(ctx, cdp).await?;
    let script = format!(
        r#"
      (async function() {{
        const sel = {sel};
        let el;
        try {{
          for (let i = 0; i < 50; i++) {{
            el = document.querySelector(sel);
            if (el) break;
            await new Promise(r => setTimeout(r, 100));
          }}
        }} catch (e) {{
          return {{ error: 'Invalid CSS selector: ' + sel + '. Use CSS selectors (#id, .class, tag).' }};
        }}
        if (!el) return {{ error: 'Element not found after 5s: ' + sel }};
        el.scrollIntoView({{ block: 'center', inline: 'center', behavior: 'instant' }});
        await new Promise(r => setTimeout(r, 50));
        const rect = el.getBoundingClientRect();
        return {{
          x: rect.left + rect.width / 2,
          y: rect.top + rect.height / 2,
          tag: el.tagName,
          text: (el.textContent || '').substring(0, 50).trim()
        }};
      }})()
    "#,
        sel = serde_json::to_string(selector)?
    );

    let result = runtime_evaluate_with_context(cdp, &script, true, true, context_id).await?;
    let value = result
        .pointer("/result/value")
        .cloned()
        .unwrap_or(Value::Null);

    if let Some(err) = value.get("error").and_then(Value::as_str) {
        bail!("{err}");
    }

    let x = value
        .get("x")
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("Element location missing x"))?;
    let y = value
        .get("y")
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("Element location missing y"))?;
    let tag = value
        .get("tag")
        .and_then(Value::as_str)
        .unwrap_or("element")
        .to_string();
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    Ok(LocatedElement { x, y, tag, text })
}

pub async fn locate_element_by_text(
    ctx: &AppContext,
    cdp: &mut CdpClient,
    text: &str,
) -> Result<LocatedElement> {
    let context_id = get_frame_context_id(ctx, cdp).await?;
    let script = format!(
        r#"
      (function() {{
        const target = {target};
        const lower = target.toLowerCase();
        let best = null;
        let bestLen = Infinity;

        function* allElements(root) {{
          for (const el of root.querySelectorAll('*')) {{
            yield el;
            if (el.shadowRoot) yield* allElements(el.shadowRoot);
          }}
        }}

        function isInteractive(el) {{
          if (!el) return false;
          return ['A','BUTTON','INPUT','SELECT','TEXTAREA','SUMMARY'].includes(el.tagName)
            || el.getAttribute('role') === 'button'
            || el.getAttribute('role') === 'link'
            || el.getAttribute('role') === 'menuitem'
            || el.getAttribute('role') === 'tab';
        }}

        function actionableAncestor(el) {{
          let cur = el;
          for (let depth = 0; cur && depth < 5; depth += 1) {{
            if (isInteractive(cur)) return cur;
            const parent = cur.parentNode;
            if (parent instanceof ShadowRoot) {{
              cur = parent.host;
            }} else {{
              cur = cur.parentElement;
            }}
          }}
          return el;
        }}

        for (const el of allElements(document)) {{
          if (el.offsetParent === null && el.tagName !== 'BODY' && el.tagName !== 'HTML') {{
            const s = getComputedStyle(el);
            if (s.display === 'none' || (s.position !== 'fixed' && s.position !== 'sticky')) continue;
          }}
          const t = (el.textContent || '').trim();
          if (!t) continue;
          const tl = t.toLowerCase();
          const exact = tl === lower;
          const has = tl.includes(lower);
          if (!exact && !has) continue;
          const clickEl = isInteractive(el) ? el : actionableAncestor(el);
          const interactive = isInteractive(clickEl);
          const len = t.length;
          if (exact) {{
            if (!best || !best.exact || (interactive && !best.interactive) || (interactive === best.interactive && len < bestLen)) {{
              best = {{ el: clickEl, exact: true, interactive, matchedText: t }}; bestLen = len;
            }}
          }} else if (has && !(best && best.exact)) {{
            if (!best || (interactive && !best.interactive) || (interactive === best.interactive && len < bestLen)) {{
              best = {{ el: clickEl, exact: false, interactive, matchedText: t }}; bestLen = len;
            }}
          }}
        }}

        if (!best) return {{ error: 'No visible element with text: ' + target }};
        const el = best.el;
        el.scrollIntoView({{ block: 'center', inline: 'center', behavior: 'instant' }});
        const rect = el.getBoundingClientRect();
        return {{
          x: rect.left + rect.width / 2,
          y: rect.top + rect.height / 2,
          tag: el.tagName,
          text: (best.matchedText || el.textContent || '').substring(0, 50).trim()
        }};
      }})()
    "#,
        target = serde_json::to_string(text)?
    );

    let result = runtime_evaluate_with_context(cdp, &script, true, false, context_id).await?;
    let value = result
        .pointer("/result/value")
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(err) = value.get("error").and_then(Value::as_str) {
        bail!("{err}");
    }
    let x = value
        .get("x")
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("Element location missing x"))?;
    let y = value
        .get("y")
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("Element location missing y"))?;
    let tag = value
        .get("tag")
        .and_then(Value::as_str)
        .unwrap_or("element")
        .to_string();
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(LocatedElement { x, y, tag, text })
}

pub async fn snapshot_tab_ids(ctx: &AppContext) -> Result<HashSet<String>> {
    Ok(get_debug_tabs(ctx)
        .await?
        .into_iter()
        .map(|tab| tab.id)
        .collect())
}

pub async fn adopt_new_tabs(
    ctx: &mut AppContext,
    before: &HashSet<String>,
    timeout: Duration,
) -> Result<Vec<DebugTab>> {
    let expected_window = ctx.load_session_state()?.window_id;
    let deadline = Instant::now() + timeout;

    loop {
        let tabs = get_debug_tabs(ctx).await?;
        let mut new_tabs = tabs
            .into_iter()
            .filter(|tab| !before.contains(&tab.id))
            .collect::<Vec<_>>();

        if let Some(window_id) = expected_window {
            let mut same_window = Vec::new();
            for tab in new_tabs {
                let Some(ws_url) = tab.web_socket_debugger_url.as_deref() else {
                    continue;
                };
                if get_window_id_for_target(ctx, ws_url).await.ok() == Some(window_id) {
                    same_window.push(tab);
                }
            }
            new_tabs = same_window;
        } else if new_tabs.len() > 1 {
            new_tabs.clear();
        }

        if !new_tabs.is_empty() {
            let mut state = ctx.load_session_state()?;
            let max_tabs = crate::config::load_config().max_tabs;
            if state.tabs.len() >= max_tabs {
                wlog!(
                    "tab limit ({max_tabs}) reached during adoption — consider closing unused tabs"
                );
            }
            for tab in &new_tabs {
                if !state.tabs.iter().any(|id| id == &tab.id) {
                    state.tabs.push(tab.id.clone());
                }
            }

            let active = new_tabs
                .iter()
                .find(|tab| tab.url.as_deref().is_some_and(|url| url != "about:blank"))
                .or_else(|| new_tabs.first())
                .map(|tab| tab.id.clone());

            if let Some(active_tab_id) = active {
                state.active_tab_id = Some(active_tab_id);
            }
            ctx.save_session_state(&state)?;
            return Ok(new_tabs);
        }

        if Instant::now() >= deadline {
            return Ok(Vec::new());
        }
        sleep(Duration::from_millis(100)).await;
    }
}

pub async fn focus_editable_element(
    cdp: &mut CdpClient,
    context_id: Option<i64>,
    selector: &str,
    select_existing: bool,
) -> Result<()> {
    let query = deep_query_expr(selector)?;
    let script = format!(
        r#"(function() {{
          const found = {query};
          if (found && found.error) return found;
          const el = found;
          if (!el) return {{ error: 'Element not found: ' + {sel} }};
          el.focus();
          if ({select_existing} && typeof el.select === 'function' && el.type !== 'password') {{
            el.select();
          }}
          return {{ ok: true }};
        }})()"#,
        query = query,
        sel = serde_json::to_string(selector)?,
        select_existing = if select_existing { "true" } else { "false" }
    );
    let result = runtime_evaluate_with_context(cdp, &script, true, false, context_id).await?;
    check_js_error(&result)?;
    Ok(())
}

pub async fn clear_editable_element(
    cdp: &mut CdpClient,
    context_id: Option<i64>,
    selector: &str,
) -> Result<()> {
    let query = deep_query_expr(selector)?;
    let script = format!(
        r#"(function() {{
          const found = {query};
          if (found && found.error) return found;
          const el = found;
          if (!el) return {{ error: 'Element not found: ' + {sel} }};
          el.focus();
          if ('value' in el) {{
            el.value = '';
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
          }} else if (el.isContentEditable) {{
            el.textContent = '';
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
          }}
          return {{ ok: true }};
        }})()"#,
        query = query,
        sel = serde_json::to_string(selector)?
    );
    let result = runtime_evaluate_with_context(cdp, &script, true, false, context_id).await?;
    check_js_error(&result)?;
    Ok(())
}

pub async fn editable_element_value(
    cdp: &mut CdpClient,
    context_id: Option<i64>,
    selector: &str,
) -> Result<String> {
    let query = deep_query_expr(selector)?;
    let script = format!(
        r#"(function() {{
          const found = {query};
          if (found && found.error) return found;
          const el = found;
          if (!el) return {{ error: 'Element not found: ' + {sel} }};
          const value = 'value' in el
            ? String(el.value || '')
            : (el.isContentEditable ? String(el.textContent || '') : String(el.textContent || ''));
          return {{ value }};
        }})()"#,
        query = query,
        sel = serde_json::to_string(selector)?
    );
    let result = runtime_evaluate_with_context(cdp, &script, true, false, context_id).await?;
    check_js_error(&result)?;
    Ok(result
        .pointer("/result/value/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

async fn type_text_via_key_events(cdp: &mut CdpClient, text: &str) -> Result<()> {
    for ch in text.chars() {
        let char_s = ch.to_string();
        cdp.send(
            "Input.dispatchKeyEvent",
            json!({ "type": "keyDown", "text": char_s, "unmodifiedText": char_s }),
        )
        .await?;
        cdp.send(
            "Input.dispatchKeyEvent",
            json!({ "type": "keyUp", "text": ch.to_string(), "unmodifiedText": ch.to_string() }),
        )
        .await?;
        sleep(Duration::from_millis(12)).await;
    }
    Ok(())
}

pub async fn type_text_verified(
    cdp: &mut CdpClient,
    context_id: Option<i64>,
    selector: &str,
    text: &str,
) -> Result<()> {
    focus_editable_element(cdp, context_id, selector, true).await?;
    type_text_via_key_events(cdp, text).await?;
    if editable_element_value(cdp, context_id, selector).await? == text {
        return Ok(());
    }

    clear_editable_element(cdp, context_id, selector).await?;
    focus_editable_element(cdp, context_id, selector, false).await?;
    cdp.send("Input.insertText", json!({ "text": text }))
        .await?;
    sleep(Duration::from_millis(50)).await;
    if editable_element_value(cdp, context_id, selector).await? == text {
        return Ok(());
    }

    let query = deep_query_expr(selector)?;
    let set_script = format!(
        r#"(function() {{
          const found = {query};
          if (found && found.error) return found;
          const el = found;
          if (!el) return {{ error: 'Element not found: ' + {sel} }};
          if ('value' in el) {{
            const proto = el.tagName === 'TEXTAREA'
              ? HTMLTextAreaElement.prototype
              : HTMLInputElement.prototype;
            const setter = Object.getOwnPropertyDescriptor(proto, 'value')?.set;
            if (setter) setter.call(el, {text});
            else el.value = {text};
            el.dispatchEvent(new InputEvent('input', {{
              bubbles: true,
              inputType: 'insertText',
              data: {text}
            }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
          }} else if (el.isContentEditable) {{
            el.textContent = {text};
            el.dispatchEvent(new InputEvent('input', {{
              bubbles: true,
              inputType: 'insertText',
              data: {text}
            }}));
          }} else {{
            return {{ error: 'Element is not editable: ' + {sel} }};
          }}
          return {{ ok: true }};
        }})()"#,
        query = query,
        sel = serde_json::to_string(selector)?,
        text = serde_json::to_string(text)?
    );
    let result = runtime_evaluate_with_context(cdp, &set_script, true, false, context_id).await?;
    check_js_error(&result)?;

    if editable_element_value(cdp, context_id, selector).await? == text {
        return Ok(());
    }

    bail!("Typed text did not stick in {selector}");
}

pub fn resolve_selector(ctx: &AppContext, input: &str) -> Result<String> {
    if input.chars().all(|c| c.is_ascii_digit()) {
        let state = ctx.load_session_state()?;
        let map = state
            .ref_map
            .ok_or_else(|| anyhow!("No ref map. Run: ax-tree -i"))?;
        let selector = map
            .get(input)
            .cloned()
            .ok_or_else(|| anyhow!("Ref {input} not found. Run: ax-tree -i to refresh."))?;
        return Ok(selector);
    }
    Ok(input.to_string())
}

#[derive(Debug)]
pub struct InteractiveData {
    pub elements: Vec<InteractiveElement>,
    pub output: String,
}

pub async fn fetch_interactive_elements(
    ctx: &mut AppContext,
    cdp: &mut CdpClient,
) -> Result<InteractiveData> {
    let current_url_result = runtime_evaluate(cdp, "location.href", true, false).await?;
    let current_url = current_url_result
        .pointer("/result/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let cache_key = cache_key_from_url(&current_url);

    let mut action_cache = load_action_cache(ctx)?;
    if let Some(cached) = action_cache.get(&cache_key).cloned() {
        if now_epoch_ms() - cached.timestamp < CACHE_TTL_MS && !cached.ref_map.is_empty() {
            let refs_to_check = cached.ref_map.values().take(3).cloned().collect::<Vec<_>>();
            let mut valid = !refs_to_check.is_empty();
            for sel in refs_to_check {
                let check = runtime_evaluate(
                    cdp,
                    &format!("!!document.querySelector({})", serde_json::to_string(&sel)?),
                    true,
                    false,
                )
                .await?;
                if !check
                    .pointer("/result/value")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    valid = false;
                    break;
                }
            }
            if valid {
                let overlay_check = runtime_evaluate(
                    cdp,
                    "document.querySelectorAll('[role=dialog],[role=alertdialog],[role=menu],[role=listbox],[aria-modal=true],[aria-modal=\"true\"],.modal,.modal-dialog,.drawer,.popover,[data-modal],[data-state=open],[data-headlessui-state~=open]').length",
                    true,
                    false,
                )
                .await?;
                let overlay_count = overlay_check
                    .pointer("/result/value")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                if overlay_count > 0 {
                    valid = false;
                }
            }
            if valid {
                let mut state = ctx.load_session_state()?;
                state.prev_elements = state.current_elements.clone();
                state.current_elements = Some(cached.elements.clone());
                state.ref_map = Some(cached.ref_map.clone());
                state.ref_map_url = Some(current_url);
                state.ref_map_timestamp = Some(cached.timestamp);
                ctx.save_session_state(&state)?;
                return Ok(InteractiveData {
                    elements: cached.elements,
                    output: cached.output,
                });
            }
        }
    }

    let script = AXTREE_INTERACTIVE_SCRIPT.replace("__SIDEKAR_SELECTOR_GEN__", SELECTOR_GEN_SCRIPT);
    let context_id = get_frame_context_id(ctx, cdp).await?;
    let result = runtime_evaluate_with_context(cdp, &script, true, false, context_id).await?;
    let items = result
        .pointer("/result/value")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut elements = Vec::new();
    let mut ref_map = HashMap::new();
    let mut lines = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        let ref_id = idx + 1;
        let selector = item
            .get("selector")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let role = item
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("element")
            .to_string();
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let value = item
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        lines.push(if name.is_empty() {
            format!("[{}] {}", ref_id, role)
        } else {
            format!("[{}] {} \"{}\"", ref_id, role, truncate(&name, 80))
        });
        ref_map.insert(ref_id.to_string(), selector);
        elements.push(InteractiveElement {
            ref_id,
            role,
            name,
            value,
        });
    }
    let mut output = lines.join("\n");
    if output.len() > 6000 {
        let boundary = output.floor_char_boundary(6000);
        output = format!("{}\n... (truncated)", &output[..boundary]);
    }
    if output.is_empty() {
        output = "(no interactive elements found)".to_string();
    }

    let mut state = ctx.load_session_state()?;
    state.prev_elements = state.current_elements.clone();
    state.current_elements = Some(elements.clone());
    state.ref_map = Some(ref_map.clone());
    state.ref_map_url = Some(current_url.clone());
    state.ref_map_timestamp = Some(now_epoch_ms());
    ctx.save_session_state(&state)?;

    action_cache.insert(
        cache_key,
        ActionCacheEntry {
            ref_map: ref_map.clone(),
            elements: elements.clone(),
            output: output.clone(),
            timestamp: now_epoch_ms(),
        },
    );
    save_action_cache(ctx, &action_cache)?;

    Ok(InteractiveData { elements, output })
}

pub fn diff_elements(
    prev: &[InteractiveElement],
    curr: &[InteractiveElement],
) -> (
    Vec<InteractiveElement>,
    Vec<InteractiveElement>,
    Vec<(InteractiveElement, InteractiveElement)>,
) {
    let prev_map = prev
        .iter()
        .map(|e| (e.ref_id, e.clone()))
        .collect::<HashMap<_, _>>();
    let curr_map = curr
        .iter()
        .map(|e| (e.ref_id, e.clone()))
        .collect::<HashMap<_, _>>();

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();

    for (ref_id, el) in &curr_map {
        if let Some(old) = prev_map.get(ref_id) {
            if old.role != el.role || old.name != el.name || old.value != el.value {
                changed.push((old.clone(), el.clone()));
            }
        } else {
            added.push(el.clone());
        }
    }
    for (ref_id, el) in &prev_map {
        if !curr_map.contains_key(ref_id) {
            removed.push(el.clone());
        }
    }
    (added, removed, changed)
}

pub fn cache_key_from_url(url: &str) -> String {
    if let Ok(parsed) = reqwest::Url::parse(url) {
        format!("{}{}", parsed.host_str().unwrap_or_default(), parsed.path())
    } else {
        url.to_string()
    }
}

pub fn load_action_cache(ctx: &AppContext) -> Result<HashMap<String, ActionCacheEntry>> {
    let path = ctx.action_cache_file();
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed reading {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("failed parsing {}", path.display()))
}

pub fn save_action_cache(
    ctx: &AppContext,
    cache: &HashMap<String, ActionCacheEntry>,
) -> Result<()> {
    let now = now_epoch_ms();
    let mut entries = cache
        .iter()
        .filter(|(_, v)| now - v.timestamp <= CACHE_TTL_MS)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| b.1.timestamp.cmp(&a.1.timestamp));
    entries.truncate(CACHE_MAX_ENTRIES);
    let pruned = entries.into_iter().collect::<HashMap<_, _>>();
    let path = ctx.action_cache_file();
    atomic_write_json(&path, &pruned)
}

/// Read-modify-write tab locks under an exclusive file lock.
/// Uses a separate `.lock` file to avoid flock+rename inode mismatch.
pub(crate) fn with_tab_locks_exclusive<F, R>(ctx: &AppContext, f: F) -> Result<R>
where
    F: FnOnce(&mut HashMap<String, TabLock>) -> Result<R>,
{
    let path = ctx.tab_locks_file();
    let lock_path = path.with_extension("lock");
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed opening lock file {}", lock_path.display()))?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("failed locking {}", lock_path.display()))?;
    let mut locks: HashMap<String, TabLock> = if path.exists() {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed reading {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("failed parsing {}", path.display()))?
    } else {
        HashMap::new()
    };
    let result = f(&mut locks)?;
    atomic_write_json(&path, &locks)?;
    Ok(result)
}

pub fn check_tab_lock(ctx: &AppContext, tab_id: &str) -> Result<Option<TabLock>> {
    let tab_id = tab_id.to_string();
    let now = now_epoch_ms();
    with_tab_locks_exclusive(ctx, |locks| {
        if let Some(lock) = locks.get(&tab_id).cloned() {
            if now.saturating_sub(lock.expires) > 0 {
                locks.remove(&tab_id);
                return Ok(None);
            }
            return Ok(Some(lock));
        }
        Ok(None)
    })
}
