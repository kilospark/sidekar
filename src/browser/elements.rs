use super::*;

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
