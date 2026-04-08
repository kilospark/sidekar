use super::*;

pub(super) async fn cmd_download(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("path");
    let mut state = ctx.load_session_state()?;
    let download_dir = state
        .download_dir
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| ctx.default_download_dir());

    match action {
        "path" => {
            let dir = args
                .get(1)
                .map(PathBuf::from)
                .unwrap_or_else(|| download_dir.clone());
            fs::create_dir_all(&dir)
                .with_context(|| format!("failed creating {}", dir.display()))?;
            state.download_dir = Some(dir.to_string_lossy().to_string());
            ctx.save_session_state(&state)?;
            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;
            cdp.send(
                "Browser.setDownloadBehavior",
                json!({
                    "behavior": "allow",
                    "downloadPath": dir.to_string_lossy().to_string()
                }),
            )
            .await?;
            out!(ctx, "Downloads will be saved to: {}", dir.display());
            cdp.close().await;
        }
        "list" => {
            if !download_dir.exists() {
                out!(ctx, "No downloads directory.");
                return Ok(());
            }
            let mut files = fs::read_dir(&download_dir)
                .with_context(|| format!("failed listing {}", download_dir.display()))?
                .filter_map(|e| e.ok())
                .collect::<Vec<_>>();
            files.sort_by_key(|e| e.file_name());
            if files.is_empty() {
                out!(ctx, "No downloaded files.");
            } else {
                for entry in files {
                    let path = entry.path();
                    let name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let stat = fs::metadata(&path)
                        .with_context(|| format!("failed stat {}", path.display()))?;
                    let size = human_size(stat.len());
                    out!(ctx, "{} ({})", name, size);
                }
            }
        }
        _ => bail!("Usage: sidekar download [path <dir>|list]"),
    }
    Ok(())
}

pub(super) async fn cmd_activate(ctx: &mut AppContext) -> Result<()> {
    let state = ctx.load_session_state()?;

    // Try per-window CDP restore first (scoped to this session's window)
    if let Some(wid) = state.window_id {
        if let Some(tab_id) = state.active_tab_id.as_ref() {
            let tabs = get_debug_tabs(ctx).await.unwrap_or_default();
            if let Some(tab) = tabs.iter().find(|t| &t.id == tab_id) {
                if let Some(ws_url) = &tab.web_socket_debugger_url {
                    if restore_window_by_id(ctx, ws_url, wid).await.is_ok() {
                        // Still need AppleScript to bring Chrome to foreground
                        if let Some(name) = state
                            .browser_name
                            .as_ref()
                            .or(ctx.launch_browser_name.as_ref())
                        {
                            let _ = activate_browser(name);
                        }
                        out!(ctx, "Brought session window to front.");
                        return Ok(());
                    }
                    // CDP failed — fall through to app-wide activate
                }
            }
        }
    }

    // Fallback: app-wide activate
    let browser_name = state
        .browser_name
        .or_else(|| find_browser().map(|b| b.name))
        .ok_or_else(|| anyhow!("Cannot determine browser."))?;
    activate_browser(&browser_name)?;
    out!(ctx, "Brought {} to front.", browser_name);
    Ok(())
}

pub(super) async fn cmd_minimize(ctx: &mut AppContext) -> Result<()> {
    let state = ctx.load_session_state()?;

    // Try per-window CDP minimize first (scoped to this session's window)
    if let Some(wid) = state.window_id {
        if let Some(tab_id) = state.active_tab_id.as_ref() {
            let tabs = get_debug_tabs(ctx).await.unwrap_or_default();
            if let Some(tab) = tabs.iter().find(|t| &t.id == tab_id) {
                if let Some(ws_url) = &tab.web_socket_debugger_url {
                    if minimize_window_by_id(ctx, ws_url, wid).await.is_ok() {
                        out!(ctx, "Minimized session window.");
                        return Ok(());
                    }
                    // CDP failed — fall through to app-wide minimize
                }
            }
        }
    }

    // Fallback: app-wide minimize
    let browser_name = state
        .browser_name
        .or_else(|| find_browser().map(|b| b.name))
        .ok_or_else(|| anyhow!("Cannot determine browser."))?;
    minimize_browser(&browser_name)?;
    out!(ctx, "Minimized {}.", browser_name);
    Ok(())
}

pub(super) async fn cmd_human_click_dispatch(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if let Some((raw_x, raw_y)) = parse_coordinates(args) {
        let (x, y) = adjust_coords_for_zoom(ctx, raw_x, raw_y);
        let tabs_before = snapshot_tab_ids(ctx).await?;
        let mut cdp = open_cdp(ctx).await?;
        prepare_cdp(ctx, &mut cdp).await?;
        human_click(&mut cdp, x, y).await?;
        out!(ctx, "Human-clicked at ({x}, {y})");
        sleep(Duration::from_millis(150)).await;
        let adopted = adopt_new_tabs(ctx, &tabs_before, Duration::from_millis(800)).await?;
        if !adopted.is_empty() {
            cdp.close().await;
            let mut adopted_cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut adopted_cdp).await?;
            out!(
                ctx,
                "Adopted {} new tab(s); switched to [{}]",
                adopted.len(),
                adopted
                    .iter()
                    .find(|tab| tab.url.as_deref().is_some_and(|url| url != "about:blank"))
                    .or_else(|| adopted.first())
                    .map(|tab| tab.id.as_str())
                    .unwrap_or("unknown")
            );
            out!(ctx, "{}", get_page_brief(&mut adopted_cdp).await?);
            adopted_cdp.close().await;
            return Ok(());
        }
        out!(ctx, "{}", get_page_brief(&mut cdp).await?);
        cdp.close().await;
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--text") {
        let text = args[1..].join(" ");
        if text.is_empty() {
            bail!("Usage: sidekar click --mode=human --text <text>");
        }
        let tabs_before = snapshot_tab_ids(ctx).await?;
        let mut cdp = open_cdp(ctx).await?;
        prepare_cdp(ctx, &mut cdp).await?;
        let loc = locate_element_by_text(ctx, &mut cdp, &text).await?;
        human_click(&mut cdp, loc.x, loc.y).await?;
        out!(
            ctx,
            "Human-clicked {} \"{}\" (text match)",
            loc.tag.to_lowercase(),
            loc.text
        );
        sleep(Duration::from_millis(150)).await;
        let adopted = adopt_new_tabs(ctx, &tabs_before, Duration::from_millis(800)).await?;
        if !adopted.is_empty() {
            cdp.close().await;
            let mut adopted_cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut adopted_cdp).await?;
            out!(
                ctx,
                "Adopted {} new tab(s); switched to [{}]",
                adopted.len(),
                adopted
                    .iter()
                    .find(|tab| tab.url.as_deref().is_some_and(|url| url != "about:blank"))
                    .or_else(|| adopted.first())
                    .map(|tab| tab.id.as_str())
                    .unwrap_or("unknown")
            );
            out!(ctx, "{}", get_page_brief(&mut adopted_cdp).await?);
            adopted_cdp.close().await;
            return Ok(());
        }
        out!(ctx, "{}", get_page_brief(&mut cdp).await?);
        cdp.close().await;
        return Ok(());
    }
    let selector = resolve_selector(ctx, &args.join(" "))?;
    cmd_human_click(ctx, &selector).await
}

pub(super) async fn cmd_human_click(ctx: &mut AppContext, selector: &str) -> Result<()> {
    let tabs_before = snapshot_tab_ids(ctx).await?;
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let loc = locate_element(ctx, &mut cdp, selector).await?;
    human_click(&mut cdp, loc.x, loc.y).await?;
    out!(
        ctx,
        "Human-clicked {} \"{}\"",
        loc.tag.to_lowercase(),
        loc.text
    );
    sleep(Duration::from_millis(150)).await;
    let adopted = adopt_new_tabs(ctx, &tabs_before, Duration::from_millis(800)).await?;
    if !adopted.is_empty() {
        cdp.close().await;
        let mut adopted_cdp = open_cdp(ctx).await?;
        prepare_cdp(ctx, &mut adopted_cdp).await?;
        out!(
            ctx,
            "Adopted {} new tab(s); switched to [{}]",
            adopted.len(),
            adopted
                .iter()
                .find(|tab| tab.url.as_deref().is_some_and(|url| url != "about:blank"))
                .or_else(|| adopted.first())
                .map(|tab| tab.id.as_str())
                .unwrap_or("unknown")
        );
        out!(ctx, "{}", get_page_brief(&mut adopted_cdp).await?);
        adopted_cdp.close().await;
        return Ok(());
    }
    out!(ctx, "{}", get_page_brief(&mut cdp).await?);
    cdp.close().await;
    Ok(())
}

pub(super) async fn cmd_human_type(ctx: &mut AppContext, selector: &str, text: &str) -> Result<()> {
    if text.is_empty() {
        bail!("Usage: sidekar type --human <selector> <text>");
    }
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let context_id = get_frame_context_id(ctx, &mut cdp).await?;
    let script = format!(
        "(function() {{ const el = document.querySelector({sel}); if (!el) return {{ error: 'Element not found' }}; el.focus(); if (el.select) el.select(); return {{ ok: true }}; }})()",
        sel = serde_json::to_string(selector)?
    );
    let r = runtime_evaluate_with_context(&mut cdp, &script, true, false, context_id).await?;
    crate::check_js_error(&r)?;
    human_type_text(&mut cdp, text, false).await?;
    out!(
        ctx,
        "Human-typed \"{}\" into {}",
        truncate(text, 50),
        selector
    );
    cdp.close().await;
    Ok(())
}

pub(super) async fn cmd_lock(ctx: &mut AppContext, ttl_seconds: Option<&str>) -> Result<()> {
    let ttl = ttl_seconds
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(300);
    let state = ctx.load_session_state()?;
    let tab_id = state
        .active_tab_id
        .ok_or_else(|| anyhow!("No active tab"))?;
    let sid = ctx.require_session_id()?.to_string();

    with_tab_locks_exclusive(ctx, |locks| {
        if let Some(lock) = locks.get(&tab_id) {
            if lock.session_id != sid && now_epoch_ms() <= lock.expires {
                let remaining = ((lock.expires - now_epoch_ms()).max(0) / 1000) as i64;
                bail!(
                    "Tab already locked by session {} (expires in {}s)",
                    lock.session_id,
                    remaining
                );
            }
        }
        locks.insert(
            tab_id.clone(),
            TabLock {
                session_id: sid.clone(),
                expires: now_epoch_ms() + ttl * 1000,
            },
        );
        Ok(())
    })?;
    out!(ctx, "Tab {} locked for {}s by session {}", tab_id, ttl, sid);
    Ok(())
}

pub(super) async fn cmd_unlock(ctx: &mut AppContext) -> Result<()> {
    let state = ctx.load_session_state()?;
    let tab_id = state
        .active_tab_id
        .ok_or_else(|| anyhow!("No active tab"))?;
    let sid = ctx.require_session_id()?.to_string();

    let msg = with_tab_locks_exclusive(ctx, |locks| match locks.get(&tab_id).cloned() {
        None => Ok("Tab is not locked.".to_string()),
        Some(l) if l.session_id != sid => {
            bail!("Tab is locked by session {}, not yours.", l.session_id)
        }
        Some(_) => {
            locks.remove(&tab_id);
            Ok(format!("Tab {} unlocked.", tab_id))
        }
    })?;
    out!(ctx, "{msg}");
    Ok(())
}

// --- State save/load (cookies + localStorage + sessionStorage) ---

pub(super) async fn cmd_state(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("");
    match action {
        "save" => {
            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;

            // Get current URL/origin
            let origin_result = runtime_evaluate(&mut cdp, "location.origin", true, false).await?;
            let origin = origin_result
                .pointer("/result/value")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let url_result = runtime_evaluate(&mut cdp, "location.href", true, false).await?;
            let url = url_result
                .pointer("/result/value")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();

            // Cookies
            let cookies_result = cdp.send("Network.getCookies", json!({})).await?;
            let cookies = cookies_result.get("cookies").cloned().unwrap_or(json!([]));

            // localStorage
            let ls_result = cdp
                .send(
                    "DOMStorage.getDOMStorageItems",
                    json!({"storageId": {"securityOrigin": origin, "isLocalStorage": true}}),
                )
                .await;
            let local_storage = ls_result
                .ok()
                .and_then(|r| r.get("entries").cloned())
                .and_then(|entries| entries.as_array().cloned())
                .map(|arr| {
                    let mut map = serde_json::Map::new();
                    for pair in &arr {
                        if let Some(pair_arr) = pair.as_array() {
                            if pair_arr.len() >= 2 {
                                let k = pair_arr[0].as_str().unwrap_or_default().to_string();
                                let v = pair_arr[1].as_str().unwrap_or_default().to_string();
                                map.insert(k, json!(v));
                            }
                        }
                    }
                    Value::Object(map)
                })
                .unwrap_or(json!({}));

            // sessionStorage
            let ss_result = cdp
                .send(
                    "DOMStorage.getDOMStorageItems",
                    json!({"storageId": {"securityOrigin": origin, "isLocalStorage": false}}),
                )
                .await;
            let session_storage = ss_result
                .ok()
                .and_then(|r| r.get("entries").cloned())
                .and_then(|entries| entries.as_array().cloned())
                .map(|arr| {
                    let mut map = serde_json::Map::new();
                    for pair in &arr {
                        if let Some(pair_arr) = pair.as_array() {
                            if pair_arr.len() >= 2 {
                                let k = pair_arr[0].as_str().unwrap_or_default().to_string();
                                let v = pair_arr[1].as_str().unwrap_or_default().to_string();
                                map.insert(k, json!(v));
                            }
                        }
                    }
                    Value::Object(map)
                })
                .unwrap_or(json!({}));

            let state_data = json!({
                "version": 1,
                "url": url,
                "origin": origin,
                "cookies": cookies,
                "localStorage": local_storage,
                "sessionStorage": session_storage,
            });

            let output_path = args.get(1).map(String::as_str).unwrap_or("");
            let file = if output_path.is_empty() {
                let sid = ctx
                    .current_session_id
                    .clone()
                    .unwrap_or_else(|| "default".to_string());
                ctx.tmp_dir().join(format!("sidekar-state-{sid}.json"))
            } else {
                PathBuf::from(output_path)
            };
            fs::write(&file, serde_json::to_string_pretty(&state_data)?)
                .with_context(|| format!("failed writing {}", file.display()))?;

            let cookie_count = cookies.as_array().map(|a| a.len()).unwrap_or(0);
            let ls_count = local_storage.as_object().map(|m| m.len()).unwrap_or(0);
            let ss_count = session_storage.as_object().map(|m| m.len()).unwrap_or(0);
            out!(
                ctx,
                "State saved to {}\n  {} cookies, {} localStorage, {} sessionStorage entries",
                file.display(),
                cookie_count,
                ls_count,
                ss_count
            );
            cdp.close().await;
        }
        "load" => {
            let path = args.get(1).context("Usage: state load <path>")?;
            let data =
                fs::read_to_string(path).with_context(|| format!("failed reading {path}"))?;
            let state_data: Value =
                serde_json::from_str(&data).with_context(|| format!("failed parsing {path}"))?;

            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;

            // Restore cookies
            cdp.send("Network.clearBrowserCookies", json!({})).await?;
            let mut cookie_count = 0usize;
            if let Some(cookies) = state_data.get("cookies").and_then(Value::as_array) {
                for cookie in cookies {
                    let _ = cdp.send("Network.setCookie", cookie.clone()).await;
                    cookie_count += 1;
                }
            }

            // Navigate to original URL if different
            let saved_url = state_data
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !saved_url.is_empty() {
                let current = runtime_evaluate(&mut cdp, "location.href", true, false).await?;
                let current_url = current
                    .pointer("/result/value")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if current_url != saved_url {
                    runtime_evaluate(
                        &mut cdp,
                        &format!(
                            "window.location.href = {}",
                            serde_json::to_string(saved_url)?
                        ),
                        false,
                        false,
                    )
                    .await?;
                    wait_for_ready_state_complete(&mut cdp, Duration::from_secs(10)).await?;
                }
            }

            let origin = state_data
                .get("origin")
                .and_then(Value::as_str)
                .unwrap_or_default();

            // Restore localStorage
            let mut ls_count = 0usize;
            if let Some(ls) = state_data.get("localStorage").and_then(Value::as_object) {
                for (k, v) in ls {
                    let val = v.as_str().unwrap_or_default();
                    let _ = cdp
                        .send(
                            "DOMStorage.setDOMStorageItem",
                            json!({
                                "storageId": {"securityOrigin": origin, "isLocalStorage": true},
                                "key": k,
                                "value": val,
                            }),
                        )
                        .await;
                    ls_count += 1;
                }
            }

            // Restore sessionStorage
            let mut ss_count = 0usize;
            if let Some(ss) = state_data.get("sessionStorage").and_then(Value::as_object) {
                for (k, v) in ss {
                    let val = v.as_str().unwrap_or_default();
                    let _ = cdp
                        .send(
                            "DOMStorage.setDOMStorageItem",
                            json!({
                                "storageId": {"securityOrigin": origin, "isLocalStorage": false},
                                "key": k,
                                "value": val,
                            }),
                        )
                        .await;
                    ss_count += 1;
                }
            }

            out!(
                ctx,
                "State loaded from {path}\n  {} cookies, {} localStorage, {} sessionStorage entries restored",
                cookie_count,
                ls_count,
                ss_count
            );
            cdp.close().await;
        }
        _ => bail!("Usage: state <save|load> [path]"),
    }
    Ok(())
}

// --- Auth vault ---

pub(super) async fn cmd_auth(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("");
    match action {
        "save" => {
            let name = args
                .get(1)
                .context("Usage: auth save <name> <username> <password> [--url=<url>] [--user-selector=<sel>] [--pass-selector=<sel>]")?;
            let username = args
                .get(2)
                .context("Missing username. Usage: auth save <name> <username> <password>")?;
            let password = args
                .get(3)
                .context("Missing password. Usage: auth save <name> <username> <password>")?;
            let url = args
                .iter()
                .find_map(|a| a.strip_prefix("--url="))
                .unwrap_or("");
            let user_sel = args
                .iter()
                .find_map(|a| a.strip_prefix("--user-selector="))
                .unwrap_or("");
            let pass_sel = args
                .iter()
                .find_map(|a| a.strip_prefix("--pass-selector="))
                .unwrap_or("");

            let entry = json!({
                "username": username,
                "password": password,
                "url": url,
                "user_selector": user_sel,
                "pass_selector": pass_sel,
            });
            let key = format!("auth:{name}");
            crate::broker::kv_set(&key, &entry.to_string(), None)?;
            out!(ctx, "Auth \"{name}\" saved (username: {username})");
        }
        "login" => {
            let name = args.get(1).context("Usage: auth login <name>")?;
            let key = format!("auth:{name}");
            let kv_entry = crate::broker::kv_get(&key)?.ok_or_else(|| {
                anyhow!("No auth entry \"{name}\". Run: auth save {name} <user> <pass>")
            })?;
            let entry: Value = serde_json::from_str(&kv_entry.value)
                .with_context(|| format!("Corrupt auth entry for \"{name}\""))?;

            let username = entry
                .get("username")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let password = entry
                .get("password")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let url = entry.get("url").and_then(Value::as_str).unwrap_or_default();
            let user_sel = entry
                .get("user_selector")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let pass_sel = entry
                .get("pass_selector")
                .and_then(Value::as_str)
                .unwrap_or_default();

            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;

            // Navigate if URL specified
            if !url.is_empty() {
                let current = runtime_evaluate(&mut cdp, "location.href", true, false).await?;
                let current_url = current
                    .pointer("/result/value")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !current_url.starts_with(url) {
                    runtime_evaluate(
                        &mut cdp,
                        &format!("window.location.href = {}", serde_json::to_string(url)?),
                        false,
                        false,
                    )
                    .await?;
                    wait_for_ready_state_complete(&mut cdp, Duration::from_secs(10)).await?;
                }
            }

            let context_id = get_frame_context_id(ctx, &mut cdp).await?;

            // Auto-detect form fields if selectors not stored
            let (final_user_sel, final_pass_sel) = if user_sel.is_empty() || pass_sel.is_empty() {
                let detect_js = r#"(() => {
                    const inputs = document.querySelectorAll('input');
                    let user = null, pass = null;
                    for (const inp of inputs) {
                        if (!inp.offsetParent && !inp.getClientRects().length) continue;
                        const type = inp.type.toLowerCase();
                        const hint = (inp.name + inp.id + inp.placeholder + (inp.autocomplete || '')).toLowerCase();
                        if (type === 'password' && !pass) { pass = inp; continue; }
                        if (!user && (type === 'email' || (type === 'text' && /user|email|login|account|name/.test(hint)))) user = inp;
                    }
                    const sel = (el) => {
                        if (!el) return '';
                        if (el.id) return '#' + el.id;
                        if (el.name) return el.tagName.toLowerCase() + '[name="' + el.name + '"]';
                        return '';
                    };
                    return { user: sel(user), pass: sel(pass) };
                })()"#;
                let result =
                    runtime_evaluate_with_context(&mut cdp, detect_js, true, true, context_id)
                        .await?;
                let detected = result
                    .pointer("/result/value")
                    .cloned()
                    .unwrap_or(Value::Null);
                let u = if user_sel.is_empty() {
                    detected
                        .get("user")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                } else {
                    user_sel.to_string()
                };
                let p = if pass_sel.is_empty() {
                    detected
                        .get("pass")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                } else {
                    pass_sel.to_string()
                };
                (u, p)
            } else {
                (user_sel.to_string(), pass_sel.to_string())
            };

            if final_user_sel.is_empty() || final_pass_sel.is_empty() {
                bail!(
                    "Could not auto-detect login form fields. Use --user-selector and --pass-selector with auth save."
                );
            }

            // Fill username
            type_text_verified(&mut cdp, context_id, &final_user_sel, username).await?;
            // Fill password
            type_text_verified(&mut cdp, context_id, &final_pass_sel, password).await?;

            // Find and click submit
            let submit_js = r#"(() => {
                const btn = document.querySelector('input[type=submit], button[type=submit]')
                    || [...document.querySelectorAll('button')].find(b => /log.?in|sign.?in|submit/i.test(b.textContent));
                if (!btn) return null;
                const rect = btn.getBoundingClientRect();
                return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
            })()"#;
            let submit_result =
                runtime_evaluate_with_context(&mut cdp, submit_js, true, true, context_id).await?;
            let submit_pos = submit_result
                .pointer("/result/value")
                .cloned()
                .unwrap_or(Value::Null);
            if let (Some(x), Some(y)) = (
                submit_pos.get("x").and_then(Value::as_f64),
                submit_pos.get("y").and_then(Value::as_f64),
            ) {
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
                out!(
                    ctx,
                    "Auth \"{name}\": filled credentials and clicked submit"
                );
            } else {
                out!(
                    ctx,
                    "Auth \"{name}\": filled credentials (no submit button found — press Enter or click manually)"
                );
            }
            sleep(Duration::from_millis(500)).await;
            out!(ctx, "{}", get_page_brief(&mut cdp).await?);
            cdp.close().await;
        }
        "list" => {
            let all = crate::broker::kv_list(None)?;
            let auth_entries: Vec<_> = all.iter().filter(|e| e.key.starts_with("auth:")).collect();
            if auth_entries.is_empty() {
                out!(ctx, "No saved auth entries.");
            } else {
                for kv in &auth_entries {
                    let name = kv.key.strip_prefix("auth:").unwrap_or(&kv.key);
                    let entry: Value = serde_json::from_str(&kv.value).unwrap_or(Value::Null);
                    let user = entry.get("username").and_then(Value::as_str).unwrap_or("?");
                    let url = entry.get("url").and_then(Value::as_str).unwrap_or("");
                    if url.is_empty() {
                        out!(ctx, "  {name} — user: {user}");
                    } else {
                        out!(ctx, "  {name} — user: {user} url: {url}");
                    }
                }
            }
        }
        "delete" => {
            let name = args.get(1).context("Usage: auth delete <name>")?;
            let key = format!("auth:{name}");
            crate::broker::kv_delete(&key)?;
            out!(ctx, "Auth \"{name}\" deleted.");
        }
        _ => bail!("Usage: auth <save|login|list|delete> [args]"),
    }
    Ok(())
}
