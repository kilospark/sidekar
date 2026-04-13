use super::*;
use crate::output::PlainOutput;

#[derive(serde::Serialize)]
struct SelectOutput {
    selected: Vec<String>,
    page_brief: String,
}

impl crate::output::CommandOutput for SelectOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "Selected: {}", self.selected.join(", "))?;
        writeln!(w, "{}", self.page_brief)?;
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct FillOutput {
    filled: usize,
    page_brief: String,
}

impl crate::output::CommandOutput for FillOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "Filled {} field(s)", self.filled)?;
        writeln!(w, "{}", self.page_brief)?;
        Ok(())
    }
}

pub(crate) async fn cmd_focus(ctx: &mut AppContext, selector: &str) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let context_id = get_frame_context_id(ctx, &mut cdp).await?;
    let script = format!(
        r#"(async function() {{
          const sel = {sel};
          let el;
          for (let i = 0; i < 50; i++) {{
            el = document.querySelector(sel);
            if (el) break;
            await new Promise(r => setTimeout(r, 100));
          }}
          if (!el) return {{ error: 'Element not found after 5s: ' + sel }};
          el.focus();
          return {{ tag: el.tagName, text: (el.textContent || '').substring(0, 50).trim() }};
        }})()"#,
        sel = serde_json::to_string(selector)?
    );
    let result = runtime_evaluate_with_context(&mut cdp, &script, true, true, context_id).await?;
    let val = result
        .pointer("/result/value")
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(err) = val.get("error").and_then(Value::as_str) {
        bail!("{err}");
    }
    let msg = format!(
        "Focused <{}> \"{}\"",
        val.get("tag")
            .and_then(Value::as_str)
            .unwrap_or("element")
            .to_lowercase(),
        val.get("text").and_then(Value::as_str).unwrap_or_default()
    );
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_clear(ctx: &mut AppContext, selector: &str) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let context_id = get_frame_context_id(ctx, &mut cdp).await?;
    let script = format!(
        r#"(async function() {{
          const sel = {sel};
          let el;
          for (let i = 0; i < 50; i++) {{
            el = document.querySelector(sel);
            if (el) break;
            await new Promise(r => setTimeout(r, 100));
          }}
          if (!el) return {{ error: 'Element not found after 5s: ' + sel }};
          el.focus();
          if ('value' in el) {{
            el.value = '';
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
          }} else if (el.isContentEditable) {{
            el.textContent = '';
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
          }}
          return {{ tag: el.tagName }};
        }})()"#,
        sel = serde_json::to_string(selector)?
    );
    let result = runtime_evaluate_with_context(&mut cdp, &script, true, true, context_id).await?;
    let val = result
        .pointer("/result/value")
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(err) = val.get("error").and_then(Value::as_str) {
        bail!("{err}");
    }
    let msg = format!(
        "Cleared {} {}",
        val.get("tag")
            .and_then(Value::as_str)
            .unwrap_or("element")
            .to_lowercase(),
        selector
    );
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_keyboard(ctx: &mut AppContext, text: &str) -> Result<()> {
    if text.is_empty() {
        bail!("Usage: sidekar keyboard <text>");
    }
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    for ch in text.chars() {
        let char_s = ch.to_string();
        cdp.send(
            "Input.dispatchKeyEvent",
            json!({ "type": "keyDown", "text": char_s, "unmodifiedText": ch.to_string() }),
        )
        .await?;
        cdp.send(
            "Input.dispatchKeyEvent",
            json!({ "type": "keyUp", "text": ch.to_string(), "unmodifiedText": ch.to_string() }),
        )
        .await?;
    }
    let msg = format!("OK keyboard \"{}\"", truncate(text, 50));
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_paste(ctx: &mut AppContext, text: &str) -> Result<()> {
    if text.is_empty() {
        bail!("Usage: sidekar paste <text>");
    }
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let context_id = get_frame_context_id(ctx, &mut cdp).await?;
    let script = format!(
        r#"(function() {{
          const el = document.activeElement;
          if (!el) return {{ error: 'No active element to paste into' }};
          const dt = new DataTransfer();
          dt.setData('text/plain', {text});
          const evt = new ClipboardEvent('paste', {{
            clipboardData: dt,
            bubbles: true,
            cancelable: true
          }});
          el.dispatchEvent(evt);
          return {{ ok: true }};
        }})()"#,
        text = serde_json::to_string(text)?
    );
    let result = runtime_evaluate_with_context(&mut cdp, &script, true, false, context_id).await?;
    crate::check_js_error(&result)?;
    let msg = format!("OK pasted \"{}\"", truncate(text, 50));
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_clipboard(
    ctx: &mut AppContext,
    html: Option<&str>,
    text: Option<&str>,
) -> Result<()> {
    let html_content = html.unwrap_or("");
    let text_content = text.unwrap_or(html_content);
    if html_content.is_empty() && text_content.is_empty() {
        bail!("Usage: sidekar clipboard --html '<h1>Hello</h1>' [--text 'Hello']");
    }
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;

    // Grant clipboard permissions for the current origin
    let context_id = get_frame_context_id(ctx, &mut cdp).await?;
    let origin_script = r#"window.location.origin"#;
    let origin_result =
        runtime_evaluate_with_context(&mut cdp, origin_script, true, false, context_id).await?;
    let origin = origin_result
        .pointer("/result/value")
        .and_then(Value::as_str)
        .unwrap_or("*");

    // Grant clipboard-write and clipboard-read permissions
    let _ = cdp
        .send(
            "Browser.grantPermissions",
            json!({
                "origin": origin,
                "permissions": ["clipboardReadWrite", "clipboardSanitizedWrite"]
            }),
        )
        .await;

    // Write HTML + plain text to clipboard via navigator.clipboard.write()
    let script = format!(
        r#"(async function() {{
          try {{
            const htmlContent = {html};
            const textContent = {text};
            const htmlBlob = new Blob([htmlContent], {{ type: 'text/html' }});
            const textBlob = new Blob([textContent], {{ type: 'text/plain' }});
            const item = new ClipboardItem({{
              'text/html': htmlBlob,
              'text/plain': textBlob
            }});
            await navigator.clipboard.write([item]);
            return {{ ok: true, html_len: htmlContent.length, text_len: textContent.length }};
          }} catch(e) {{
            return {{ error: e.message }};
          }}
        }})()"#,
        html = serde_json::to_string(html_content)?,
        text = serde_json::to_string(text_content)?
    );
    let result = runtime_evaluate_with_context(&mut cdp, &script, true, true, context_id).await?;
    crate::check_js_error(&result)?;

    // Dispatch paste shortcut: Cmd+V on macOS, Ctrl+V on Linux
    let (mod_key, mod_code, mod_vk, mod_flag) = if cfg!(target_os = "macos") {
        ("Meta", "MetaLeft", 91, 4)
    } else {
        ("Control", "ControlLeft", 17, 2)
    };
    cdp.send(
        "Input.dispatchKeyEvent",
        json!({
            "type": "keyDown",
            "key": mod_key,
            "code": mod_code,
            "windowsVirtualKeyCode": mod_vk,
            "modifiers": mod_flag
        }),
    )
    .await?;
    cdp.send(
        "Input.dispatchKeyEvent",
        json!({
            "type": "keyDown",
            "key": "v",
            "code": "KeyV",
            "windowsVirtualKeyCode": 86,
            "modifiers": mod_flag,
            "commands": ["paste"]
        }),
    )
    .await?;
    cdp.send(
        "Input.dispatchKeyEvent",
        json!({
            "type": "keyUp",
            "key": "v",
            "code": "KeyV",
            "windowsVirtualKeyCode": 86,
            "modifiers": mod_flag
        }),
    )
    .await?;
    cdp.send(
        "Input.dispatchKeyEvent",
        json!({
            "type": "keyUp",
            "key": mod_key,
            "code": mod_code,
            "windowsVirtualKeyCode": mod_vk,
            "modifiers": 0
        }),
    )
    .await?;

    let html_len = html_content.len();
    let has_html = !html_content.is_empty();
    let msg = format!(
        "OK clipboard paste ({} chars{})",
        text_content.len(),
        if has_html {
            format!(", {} chars HTML", html_len)
        } else {
            String::new()
        }
    );
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_inserttext(ctx: &mut AppContext, text: &str) -> Result<()> {
    if text.is_empty() {
        bail!("Usage: sidekar insert-text <text>");
    }
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    cdp.send("Input.insertText", json!({ "text": text }))
        .await?;
    let msg = format!("OK insert-text \"{}\"", truncate(text, 50));
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_select(
    ctx: &mut AppContext,
    selector: &str,
    values: &[String],
) -> Result<()> {
    if values.is_empty() {
        bail!("Usage: sidekar select <selector> <value> [value2...]");
    }
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let context_id = get_frame_context_id(ctx, &mut cdp).await?;
    let script = format!(
        r#"(async function() {{
          const sel = {sel};
          const vals = {vals};
          let el;
          for (let i = 0; i < 50; i++) {{
            el = document.querySelector(sel);
            if (el) break;
            await new Promise(r => setTimeout(r, 100));
          }}
          if (!el) return {{ error: 'Element not found after 5s: ' + sel }};
          if (el.tagName !== 'SELECT') return {{ error: 'Element is not a <select>: ' + sel }};
          const matched = [];
          for (const opt of el.options) {{
            const match = vals.some(v => opt.value === v || opt.textContent.trim() === v || opt.label === v);
            opt.selected = match;
            if (match) matched.push(opt.textContent.trim() || opt.value);
          }}
          el.dispatchEvent(new Event('input', {{ bubbles: true }}));
          el.dispatchEvent(new Event('change', {{ bubbles: true }}));
          if (matched.length === 0) return {{ error: 'No options matched: ' + vals.join(', ') }};
          return {{ selected: matched }};
        }})()"#,
        sel = serde_json::to_string(selector)?,
        vals = serde_json::to_string(values)?
    );
    let result = runtime_evaluate_with_context(&mut cdp, &script, true, true, context_id).await?;
    crate::check_js_error(&result)?;
    let selected = result
        .pointer("/result/value/selected")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let page_brief = get_page_brief(&mut cdp).await?;
    let output = SelectOutput {
        selected,
        page_brief,
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_upload(
    ctx: &mut AppContext,
    selector: &str,
    file_paths: &[String],
) -> Result<()> {
    if file_paths.is_empty() {
        bail!("Usage: sidekar upload <selector> <file> [file2...]");
    }
    let resolved = file_paths
        .iter()
        .map(|f| fs::canonicalize(f).unwrap_or_else(|_| PathBuf::from(f)))
        .collect::<Vec<_>>();
    for f in &resolved {
        if !f.exists() {
            bail!("File not found: {}", f.display());
        }
    }

    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    cdp.send("DOM.enable", json!({})).await?;
    let doc = cdp.send("DOM.getDocument", json!({})).await?;
    let root = doc
        .pointer("/root/nodeId")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("DOM root node not found"))?;
    let node = cdp
        .send(
            "DOM.querySelector",
            json!({ "nodeId": root, "selector": selector }),
        )
        .await?;
    let node_id = node
        .get("nodeId")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("Element not found: {selector}"))?;

    cdp.send(
        "DOM.setFileInputFiles",
        json!({
            "nodeId": node_id,
            "files": resolved.iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>()
        }),
    )
    .await?;

    let msg = format!(
        "Uploaded {} file(s) to {}: {}",
        resolved.len(),
        selector,
        resolved
            .iter()
            .map(|p| p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_fill(ctx: &mut AppContext, fields: &[(String, String)]) -> Result<()> {
    if fields.is_empty() {
        bail!("Usage: sidekar fill requires at least one field");
    }
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let context_id = get_frame_context_id(ctx, &mut cdp).await?;

    let mut filled = 0usize;
    for (selector, value) in fields {
        let resolved = resolve_selector(ctx, selector)?;
        type_text_verified(&mut cdp, context_id, &resolved, value).await?;
        filled += 1;
    }

    let page_brief = get_page_brief(&mut cdp).await?;
    let output = FillOutput { filled, page_brief };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_dialog(
    ctx: &mut AppContext,
    action: Option<&str>,
    extra_args: &[String],
) -> Result<()> {
    let action = action.unwrap_or_default().to_lowercase();
    if !matches!(action.as_str(), "accept" | "dismiss") {
        bail!("Usage: sidekar dialog <accept|dismiss> [prompt-text]");
    }
    let accept = action == "accept";
    let prompt_text = extra_args.join(" ");
    let mut state = ctx.load_session_state()?;
    state.dialog_handler = Some(DialogHandler {
        accept,
        prompt_text: prompt_text.clone(),
    });
    ctx.save_session_state(&state)?;
    let msg = if prompt_text.is_empty() {
        format!(
            "Dialog handler set: will {} the next dialog",
            if accept { "accept" } else { "dismiss" }
        )
    } else {
        format!(
            "Dialog handler set: will {} the next dialog with text: \"{}\"",
            if accept { "accept" } else { "dismiss" },
            prompt_text
        )
    };
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
    Ok(())
}
