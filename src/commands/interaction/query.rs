use super::*;
use crate::output::PlainOutput;

#[derive(serde::Serialize)]
struct FindOutput {
    best: Option<FindMatch>,
    also: Vec<FindMatch>,
}

#[derive(serde::Serialize)]
struct FindMatch {
    ref_id: usize,
    role: String,
    name: String,
    confidence: Option<String>,
    score: f64,
}

impl crate::output::CommandOutput for FindOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if let Some(b) = &self.best {
            writeln!(
                w,
                "Best: [{}] {} \"{}\" ({} confidence, score:{:.2})",
                b.ref_id,
                b.role,
                b.name,
                b.confidence.as_deref().unwrap_or(""),
                b.score
            )?;
        }
        if !self.also.is_empty() {
            writeln!(w, "Also:")?;
            for m in &self.also {
                writeln!(w, "  [{}] {} \"{}\" ({:.2})", m.ref_id, m.role, m.name, m.score)?;
            }
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct FindRoleOutput {
    matches: Vec<FindRoleRow>,
}

#[derive(serde::Serialize)]
struct FindRoleRow {
    ref_id: usize,
    role: String,
    name: String,
}

impl crate::output::CommandOutput for FindRoleOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        for m in &self.matches {
            writeln!(w, "[{}] {} \"{}\"", m.ref_id, m.role, truncate(&m.name, 80))?;
        }
        writeln!(w, "{} match(es)", self.matches.len())?;
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct FindTextOutput {
    matches: Vec<FindTextRow>,
}

#[derive(serde::Serialize)]
struct FindTextRow {
    tag: String,
    text: String,
    selector: String,
}

impl crate::output::CommandOutput for FindTextOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        for m in &self.matches {
            writeln!(w, "<{}> \"{}\" — {}", m.tag, truncate(&m.text, 60), m.selector)?;
        }
        writeln!(w, "{} match(es)", self.matches.len())?;
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct FindLabelOutput {
    matches: Vec<FindLabelRow>,
}

#[derive(serde::Serialize)]
struct FindLabelRow {
    tag: String,
    label: String,
    selector: String,
}

impl crate::output::CommandOutput for FindLabelOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        for m in &self.matches {
            writeln!(
                w,
                "<{}> label=\"{}\" — {}",
                m.tag,
                truncate(&m.label, 60),
                m.selector
            )?;
        }
        writeln!(w, "{} match(es)", self.matches.len())?;
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct ResolveOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    href: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    form_action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    src: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    onclick: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    tag: String,
    text: String,
}

impl crate::output::CommandOutput for ResolveOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if let Some(v) = &self.href {
            writeln!(w, "href: {v}")?;
        }
        if let Some(v) = &self.action {
            writeln!(w, "action: {v}")?;
        }
        if let Some(v) = &self.form_action {
            writeln!(w, "formAction: {v}")?;
        }
        if let Some(v) = &self.src {
            writeln!(w, "src: {v}")?;
        }
        if let Some(v) = &self.onclick {
            writeln!(w, "onclick: {v}")?;
        }
        if let Some(v) = &self.target {
            writeln!(w, "target: {v}")?;
        }
        if !self.text.is_empty() {
            writeln!(w, "element: <{}> \"{}\"", self.tag, self.text)?;
        } else {
            writeln!(w, "element: <{}>", self.tag)?;
        }
        Ok(())
    }
}

pub(crate) async fn cmd_eval(ctx: &mut AppContext, expression: &str) -> Result<()> {
    if expression.is_empty() {
        bail!("Usage: sidekar eval <js-expression>");
    }
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let context_id = get_frame_context_id(ctx, &mut cdp).await?;
    let result =
        runtime_evaluate_with_context(&mut cdp, expression, false, true, context_id).await?;

    let r = result.get("result").cloned().unwrap_or(Value::Null);
    let r_type = r.get("type").and_then(Value::as_str).unwrap_or("");
    if r_type == "undefined" {
        cdp.close().await;
        return Ok(());
    }
    let msg: String = if r_type == "object" {
        if let Some(object_id) = r.get("objectId").and_then(Value::as_str) {
            let ser = cdp
                .send(
                    "Runtime.callFunctionOn",
                    json!({
                        "objectId": object_id,
                        "functionDeclaration": "function() { return JSON.stringify(this, (k, v) => v instanceof HTMLElement ? v.outerHTML.slice(0, 200) : v, 2); }",
                        "returnByValue": true
                    }),
                )
                .await?;
            if let Some(v) = ser.pointer("/result/value").and_then(Value::as_str) {
                v.to_string()
            } else {
                r.get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("(object)")
                    .to_string()
            }
        } else {
            r.get("description")
                .and_then(Value::as_str)
                .unwrap_or("(object)")
                .to_string()
        }
    } else if let Some(v) = r.get("value") {
        match v {
            Value::String(s) => s.clone(),
            _ => v.to_string(),
        }
    } else {
        r.get("description")
            .and_then(Value::as_str)
            .unwrap_or("(value)")
            .to_string()
    };
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_observe(ctx: &mut AppContext) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let data = fetch_interactive_elements(ctx, &mut cdp).await?;
    if data.elements.is_empty() {
        out!(
            ctx,
            "{}",
            crate::output::to_string(&PlainOutput::new("(no interactive elements found)"))?
        );
        cdp.close().await;
        return Ok(());
    }
    let mut observe_buf = String::new();
    for el in &data.elements {
        let desc = if el.name.is_empty() {
            el.role.clone()
        } else {
            format!("{} \"{}\"", el.role, truncate(&el.name, 60))
        };
        let cmd = match el.role.as_str() {
            "textbox" | "searchbox" => format!("type {} <text>", el.ref_id),
            "combobox" | "listbox" => format!("select {} <value>", el.ref_id),
            "slider" | "spinbutton" => format!("type {} <value>", el.ref_id),
            _ => format!("click {}", el.ref_id),
        };
        observe_buf.push_str(&format!("[{}] {}  — {}\n", el.ref_id, cmd, desc));
    }
    out!(
        ctx,
        "{}",
        crate::output::to_string(&PlainOutput::new(observe_buf.trim_end().to_string()))?
    );
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_find(ctx: &mut AppContext, query: &str) -> Result<()> {
    if query.trim().is_empty() {
        bail!(
            "Usage: sidekar find <query>\n       find --role <role> [name]\n       find --text <text>\n       find --label <label>\n       find --testid <id>"
        );
    }

    // Detect structured locator flags
    let parts: Vec<&str> = query.split_whitespace().collect();
    match parts.first().copied() {
        Some("--role") => return cmd_find_by_role(ctx, &parts[1..]).await,
        Some("--text") => return cmd_find_by_text(ctx, &parts[1..].join(" ")).await,
        Some("--label") => return cmd_find_by_label(ctx, &parts[1..].join(" ")).await,
        Some("--testid") => return cmd_find_by_testid(ctx, &parts[1..].join(" ")).await,
        _ => {}
    }

    // Existing fuzzy search
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let data = fetch_interactive_elements(ctx, &mut cdp).await?;
    if data.elements.is_empty() {
        bail!("No interactive elements found. Navigate to a page first.");
    }

    let stopwords: HashSet<&str> = [
        "the", "a", "an", "to", "for", "of", "in", "on", "is", "it", "and", "or", "this", "that",
    ]
    .into_iter()
    .collect();

    let tokenize = |s: &str| -> HashSet<String> {
        s.to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
            .collect::<String>()
            .split_whitespace()
            .filter(|t| t.len() > 1 && !stopwords.contains(*t))
            .map(|s| s.to_string())
            .collect()
    };

    let query_tokens = tokenize(query);
    if query_tokens.is_empty() {
        bail!("Query too vague. Use descriptive terms like \"search input\" or \"submit button\".");
    }

    let mut scored = Vec::<(usize, f64, InteractiveElement)>::new();
    for el in &data.elements {
        let text = format!("{} {} {}", el.role, el.name, el.value);
        let el_tokens = tokenize(&text);
        if el_tokens.is_empty() {
            continue;
        }
        let mut intersection = 0.0f64;
        for t in &query_tokens {
            if el_tokens.contains(t) {
                intersection += 1.0;
            } else if el_tokens.iter().any(|et| et.contains(t) || t.contains(et)) {
                intersection += 0.5;
            }
        }
        let union_size = query_tokens.union(&el_tokens).count() as f64;
        let score = if union_size > 0.0 {
            intersection / union_size
        } else {
            0.0
        };
        if score > 0.0 {
            scored.push((el.ref_id, score, el.clone()));
        }
    }

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let top = scored.into_iter().take(5).collect::<Vec<_>>();
    if top.is_empty() {
        bail!("No elements match \"{query}\". Try: ax-tree -i");
    }

    let (best_ref, best_score, best_el) = &top[0];
    let confidence = if *best_score >= 0.5 {
        "high"
    } else if *best_score >= 0.25 {
        "medium"
    } else {
        "low"
    };
    let best = FindMatch {
        ref_id: *best_ref,
        role: best_el.role.clone(),
        name: best_el.name.clone(),
        confidence: Some(confidence.to_string()),
        score: *best_score,
    };
    let also = top
        .iter()
        .skip(1)
        .map(|(r, s, e)| FindMatch {
            ref_id: *r,
            role: e.role.clone(),
            name: e.name.clone(),
            confidence: None,
            score: *s,
        })
        .collect();
    let output = FindOutput {
        best: Some(best),
        also,
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    cdp.close().await;
    Ok(())
}

// --- Semantic locator strategies ---

async fn cmd_find_by_role(ctx: &mut AppContext, args: &[&str]) -> Result<()> {
    let role = args.first().context("Usage: find --role <role> [name]")?;
    let name_filter = if args.len() > 1 {
        Some(args[1..].join(" ").to_lowercase())
    } else {
        None
    };
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let data = fetch_interactive_elements(ctx, &mut cdp).await?;
    let role_lower = role.to_lowercase();
    let matches: Vec<_> = data
        .elements
        .iter()
        .filter(|el| el.role.to_lowercase() == role_lower)
        .filter(|el| {
            name_filter
                .as_ref()
                .is_none_or(|n| el.name.to_lowercase().contains(n))
        })
        .collect();
    if matches.is_empty() {
        bail!(
            "No elements with role \"{role}\"{}. Try: find --role button",
            name_filter
                .as_ref()
                .map(|n| format!(" matching \"{n}\""))
                .unwrap_or_default()
        );
    }
    let rows: Vec<FindRoleRow> = matches
        .iter()
        .map(|el| FindRoleRow {
            ref_id: el.ref_id,
            role: el.role.clone(),
            name: el.name.clone(),
        })
        .collect();
    let output = FindRoleOutput { matches: rows };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    cdp.close().await;
    Ok(())
}

async fn cmd_find_by_text(ctx: &mut AppContext, text: &str) -> Result<()> {
    if text.is_empty() {
        bail!("Usage: find --text <visible text>");
    }
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let context_id = get_frame_context_id(ctx, &mut cdp).await?;
    let escaped = serde_json::to_string(text)?;
    let js = format!(
        r#"(() => {{
            const text = {escaped};
            const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
            const results = [];
            while (walker.nextNode()) {{
                if (walker.currentNode.textContent.trim().toLowerCase().includes(text.toLowerCase())) {{
                    const el = walker.currentNode.parentElement;
                    if (!el) continue;
                    const tag = el.tagName.toLowerCase();
                    const rect = el.getBoundingClientRect();
                    if (rect.width === 0 && rect.height === 0) continue;
                    results.push({{
                        tag: tag,
                        text: el.textContent.trim().slice(0, 100),
                        selector: el.id ? '#' + el.id : tag + (el.className ? '.' + el.className.split(' ').filter(Boolean).join('.') : ''),
                    }});
                    if (results.length >= 5) break;
                }}
            }}
            return results;
        }})()"#
    );
    let result = runtime_evaluate_with_context(&mut cdp, &js, true, true, context_id).await?;
    let items = result
        .pointer("/result/value")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if items.is_empty() {
        bail!("No elements containing text \"{text}\"");
    }
    let rows: Vec<FindTextRow> = items
        .iter()
        .map(|item| FindTextRow {
            tag: item.get("tag").and_then(Value::as_str).unwrap_or("?").to_string(),
            text: item.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
            selector: item.get("selector").and_then(Value::as_str).unwrap_or("").to_string(),
        })
        .collect();
    let output = FindTextOutput { matches: rows };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    cdp.close().await;
    Ok(())
}

async fn cmd_find_by_label(ctx: &mut AppContext, label: &str) -> Result<()> {
    if label.is_empty() {
        bail!("Usage: find --label <label text>");
    }
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let context_id = get_frame_context_id(ctx, &mut cdp).await?;
    let escaped = serde_json::to_string(label)?;
    let js = format!(
        r#"(() => {{
            const text = {escaped}.toLowerCase();
            const results = [];
            // Check <label> elements
            for (const lbl of document.querySelectorAll('label')) {{
                if (!lbl.textContent.toLowerCase().includes(text)) continue;
                let input = lbl.control;
                if (!input && lbl.htmlFor) input = document.getElementById(lbl.htmlFor);
                if (!input) input = lbl.querySelector('input,textarea,select');
                if (input) {{
                    results.push({{
                        tag: input.tagName.toLowerCase(),
                        type: input.type || '',
                        label: lbl.textContent.trim().slice(0, 80),
                        selector: input.id ? '#' + input.id : 'input',
                    }});
                }}
            }}
            // Check aria-label
            for (const el of document.querySelectorAll('[aria-label]')) {{
                if (el.getAttribute('aria-label').toLowerCase().includes(text)) {{
                    results.push({{
                        tag: el.tagName.toLowerCase(),
                        type: el.type || '',
                        label: el.getAttribute('aria-label'),
                        selector: el.id ? '#' + el.id : el.tagName.toLowerCase(),
                    }});
                }}
            }}
            return results.slice(0, 5);
        }})()"#
    );
    let result = runtime_evaluate_with_context(&mut cdp, &js, true, true, context_id).await?;
    let items = result
        .pointer("/result/value")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if items.is_empty() {
        bail!("No elements with label matching \"{label}\"");
    }
    let rows: Vec<FindLabelRow> = items
        .iter()
        .map(|item| FindLabelRow {
            tag: item.get("tag").and_then(Value::as_str).unwrap_or("?").to_string(),
            label: item.get("label").and_then(Value::as_str).unwrap_or("").to_string(),
            selector: item.get("selector").and_then(Value::as_str).unwrap_or("").to_string(),
        })
        .collect();
    let output = FindLabelOutput { matches: rows };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    cdp.close().await;
    Ok(())
}

async fn cmd_find_by_testid(ctx: &mut AppContext, testid: &str) -> Result<()> {
    if testid.is_empty() {
        bail!("Usage: find --testid <data-testid value>");
    }
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let context_id = get_frame_context_id(ctx, &mut cdp).await?;
    let escaped = serde_json::to_string(testid)?;
    let js = format!(
        r#"(() => {{
            const id = {escaped};
            const el = document.querySelector('[data-testid="' + id + '"]');
            if (!el) return null;
            return {{
                tag: el.tagName.toLowerCase(),
                text: (el.textContent || '').trim().slice(0, 100),
                selector: '[data-testid="' + id + '"]',
            }};
        }})()"#
    );
    let result = runtime_evaluate_with_context(&mut cdp, &js, true, true, context_id).await?;
    let value = result
        .pointer("/result/value")
        .cloned()
        .unwrap_or(Value::Null);
    if value.is_null() {
        bail!("No element with data-testid=\"{testid}\"");
    }
    let tag = value.get("tag").and_then(Value::as_str).unwrap_or("?");
    let text = value.get("text").and_then(Value::as_str).unwrap_or("");
    let sel = value.get("selector").and_then(Value::as_str).unwrap_or("");
    let msg = format!(
        "<{tag}> testid=\"{testid}\" \"{}\" — {sel}",
        truncate(text, 60)
    );
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_resolve(ctx: &mut AppContext, selector: &str) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let context_id = get_frame_context_id(ctx, &mut cdp).await?;

    let js = format!(
        r#"(() => {{
            const el = document.querySelector({sel});
            if (!el) return JSON.stringify({{error: "Element not found"}});
            const result = {{}};
            if (el.href) result.href = el.href;
            if (el.action) result.action = el.action;
            if (el.formAction) result.formAction = el.formAction;
            if (el.src) result.src = el.src;
            const onclick = el.getAttribute('onclick');
            if (onclick) result.onclick = onclick;
            const target = el.getAttribute('target');
            if (target) result.target = target;
            result.tagName = el.tagName.toLowerCase();
            result.text = (el.textContent || '').trim().slice(0, 200);
            return JSON.stringify(result);
        }})()"#,
        sel = serde_json::to_string(selector)?
    );
    let result = runtime_evaluate_with_context(&mut cdp, &js, true, false, context_id).await?;
    let value = result
        .pointer("/result/value")
        .and_then(Value::as_str)
        .unwrap_or("{}");

    let parsed: Value = serde_json::from_str(value).unwrap_or(json!({}));
    if let Some(err) = parsed.get("error").and_then(Value::as_str) {
        bail!("{err}: {selector}");
    }

    let output = ResolveOutput {
        href: parsed.get("href").and_then(Value::as_str).map(String::from),
        action: parsed.get("action").and_then(Value::as_str).map(String::from),
        form_action: parsed.get("formAction").and_then(Value::as_str).map(String::from),
        src: parsed.get("src").and_then(Value::as_str).map(String::from),
        onclick: parsed.get("onclick").and_then(Value::as_str).map(String::from),
        target: parsed.get("target").and_then(Value::as_str).map(String::from),
        tag: parsed
            .get("tagName")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string(),
        text: parsed
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);

    cdp.close().await;
    Ok(())
}
