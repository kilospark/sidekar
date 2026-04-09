use super::*;

pub async fn open_cdp(ctx: &mut AppContext) -> Result<CdpClient> {
    match open_cdp_once(ctx).await {
        Ok(cdp) => Ok(cdp),
        Err(first_err) => {
            let msg = first_err.to_string();
            if msg.contains("WebSocket closed")
                || msg.contains("Connection refused")
                || msg.contains("failed to connect")
                || msg.contains("daemon")
            {
                wlog!("CDP connection failed ({msg}), retrying...");
                sleep(Duration::from_millis(500)).await;
                open_cdp_once(ctx)
                    .await
                    .with_context(|| format!("CDP retry also failed (original: {msg})"))
            } else {
                Err(first_err)
            }
        }
    }
}

async fn open_cdp_once(ctx: &mut AppContext) -> Result<CdpClient> {
    let tab = connect_to_tab(ctx).await?;
    if let Some(lock) = check_tab_lock(ctx, &tab.id)? {
        let sid = ctx.require_session_id()?;
        if lock.session_id != sid {
            let remaining = ((lock.expires - now_epoch_ms()).max(0) / 1000) as i64;
            bail!(
                "Tab is locked by session {} (expires in {}s). Use a different tab or wait.",
                lock.session_id,
                remaining
            );
        }
    }
    let ws_url = tab
        .web_socket_debugger_url
        .ok_or_else(|| anyhow!("No active tab for this session. Navigate to a URL first."))?;

    if daemon::is_running() {
        match cdp_proxy::DaemonCdpProxy::connect(&ws_url).await {
            Ok(proxy) => return Ok(CdpClient::Proxied(proxy)),
            Err(_) => {}
        }
    }

    Ok(CdpClient::Direct(DirectCdp::connect(&ws_url).await?))
}

pub async fn connect_to_tab(ctx: &mut AppContext) -> Result<DebugTab> {
    fn format_tab_candidates(tabs: &[DebugTab], owned_ids: &[String]) -> String {
        let owned = tabs
            .iter()
            .filter(|t| owned_ids.iter().any(|id| id == &t.id))
            .take(5)
            .map(|t| {
                let label = t
                    .title
                    .as_deref()
                    .or(t.url.as_deref())
                    .unwrap_or("(untitled)");
                format!("{} ({label})", t.id)
            })
            .collect::<Vec<_>>();
        if owned.is_empty() {
            "none".to_string()
        } else {
            owned.join(", ")
        }
    }

    if let Some(ref target_id) = ctx.override_tab_id {
        let tabs = get_debug_tabs(ctx).await?;
        let tab = tabs
            .iter()
            .find(|t| t.id == *target_id)
            .cloned()
            .ok_or_else(|| anyhow!("Tab not found: {target_id}"))?;
        if tab.web_socket_debugger_url.is_none() {
            bail!("Tab {target_id} has no webSocketDebuggerUrl");
        }
        return Ok(tab);
    }

    let mut state = ctx.load_session_state()?;
    let tabs = get_debug_tabs(ctx).await?;

    let live_ids: HashSet<&str> = tabs.iter().map(|t| t.id.as_str()).collect();
    let before = state.tabs.len();
    state.tabs.retain(|id| live_ids.contains(id.as_str()));
    if state.tabs.len() < before {
        wlog!(
            "Pruned {} stale tab ID(s) from session state",
            before - state.tabs.len()
        );
    }
    ctx.save_session_state(&state)?;

    let selected = if let Some(active_id) = state.active_tab_id.clone() {
        let tab = tabs
            .iter()
            .find(|t| t.id == active_id && t.web_socket_debugger_url.is_some())
            .cloned();

        if let Some(tab) = tab {
            tab
        } else {
            state.active_tab_id = None;
            ctx.save_session_state(&state)?;
            if state.tabs.is_empty() {
                bail!(
                    "Active tab {active_id} is gone and this session has no remaining tabs. Run `sidekar new-tab` or pass `--tab <id>`."
                );
            }
            let remaining = format_tab_candidates(&tabs, &state.tabs);
            bail!(
                "Active tab {active_id} is gone. Remaining session tabs: {remaining}. Run `sidekar tab <id>` or pass `--tab <id>`."
            );
        }
    } else if state.tabs.is_empty() {
        bail!("No active tab for this session. Run `sidekar new-tab` or pass `--tab <id>`.");
    } else {
        let remaining = format_tab_candidates(&tabs, &state.tabs);
        bail!(
            "No active tab is selected for this session. Remaining session tabs: {remaining}. Run `sidekar tab <id>` or pass `--tab <id>`."
        );
    };
    state.active_tab_id = Some(selected.id.clone());
    ctx.save_session_state(&state)?;

    Ok(selected)
}

pub async fn verify_cdp_ready(ctx: &AppContext) -> Result<()> {
    let tabs = get_debug_tabs(ctx).await?;
    let tab = tabs.first().ok_or_else(|| anyhow!("No tabs available"))?;
    let ws_url = tab
        .web_socket_debugger_url
        .as_ref()
        .ok_or_else(|| anyhow!("No WebSocket URL"))?;
    let mut cdp = DirectCdp::connect(ws_url).await?;
    let result = cdp.send("Browser.getVersion", json!({})).await;
    cdp.close().await;
    result.map(|_| ())
}

pub async fn get_debug_tabs(ctx: &AppContext) -> Result<Vec<DebugTab>> {
    let body = http_get_text(ctx, "/json").await?;
    serde_json::from_str::<Vec<DebugTab>>(&body).context("Failed to parse Chrome debug info")
}

pub async fn create_new_tab(ctx: &AppContext, url: Option<&str>) -> Result<DebugTab> {
    let suffix = match url {
        Some(raw) if !raw.is_empty() => {
            let encoded = urlencoding::encode(raw);
            format!("/json/new?{encoded}")
        }
        _ => "/json/new".to_string(),
    };
    let body = http_put_text(ctx, &suffix).await?;
    serde_json::from_str::<DebugTab>(&body).context("Failed to create new tab")
}

pub async fn create_new_window(ctx: &AppContext, url: Option<&str>) -> Result<DebugTab> {
    let tabs = get_debug_tabs(ctx).await?;
    let any_tab = tabs
        .first()
        .ok_or_else(|| anyhow!("No existing tab to connect through"))?;
    let ws_url = any_tab
        .web_socket_debugger_url
        .as_ref()
        .ok_or_else(|| anyhow!("No WebSocket URL for existing tab"))?;
    let mut cdp = DirectCdp::connect(ws_url).await?;
    let result = cdp
        .send(
            "Target.createTarget",
            json!({
                "url": url.unwrap_or("about:blank"),
                "newWindow": true
            }),
        )
        .await?;

    let target_id = result
        .get("targetId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("No targetId in createTarget response"))?;

    cdp.close().await;

    for _ in 0..5 {
        let all_tabs = get_debug_tabs(ctx).await?;
        if let Some(tab) = all_tabs.into_iter().find(|t| t.id == target_id) {
            return Ok(tab);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    bail!("New window tab not found in tab list after retries")
}

pub async fn detect_browser_from_port(ctx: &AppContext) -> Option<String> {
    let body = http_get_text(ctx, "/json/version").await.ok()?;
    let info: Value = serde_json::from_str(&body).ok()?;
    let browser = info.get("Browser").and_then(Value::as_str).unwrap_or("");
    let user_agent = info.get("User-Agent").and_then(Value::as_str).unwrap_or("");

    let name = if user_agent.contains("Edg/") {
        "Microsoft Edge"
    } else if user_agent.contains("Brave/") || user_agent.contains("brave") {
        "Brave Browser"
    } else if user_agent.contains("OPR/") || user_agent.contains("Opera") {
        "Opera"
    } else if user_agent.contains("Vivaldi/") {
        "Vivaldi"
    } else if user_agent.contains("Arc/") || user_agent.contains("arc ") {
        "Arc"
    } else if browser.starts_with("Chrome/") || browser.starts_with("HeadlessChrome/") {
        "Google Chrome"
    } else if browser.starts_with("Chromium/") {
        "Chromium"
    } else {
        return None;
    };
    Some(name.to_string())
}

pub async fn get_window_id_for_target(_ctx: &AppContext, tab_ws_url: &str) -> Result<i64> {
    let mut cdp = DirectCdp::connect(tab_ws_url).await?;
    let result = cdp.send("Browser.getWindowForTarget", json!({})).await?;
    cdp.close().await;
    result
        .get("windowId")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("No windowId in Browser.getWindowForTarget response"))
}

pub async fn minimize_window_by_id(
    _ctx: &AppContext,
    tab_ws_url: &str,
    window_id: i64,
) -> Result<()> {
    let mut cdp = DirectCdp::connect(tab_ws_url).await?;
    cdp.send(
        "Browser.setWindowBounds",
        json!({"windowId": window_id, "bounds": {"windowState": "minimized"}}),
    )
    .await?;
    cdp.close().await;
    Ok(())
}

pub async fn restore_window_by_id(
    _ctx: &AppContext,
    tab_ws_url: &str,
    window_id: i64,
) -> Result<()> {
    let mut cdp = DirectCdp::connect(tab_ws_url).await?;
    cdp.send(
        "Browser.setWindowBounds",
        json!({"windowId": window_id, "bounds": {"windowState": "normal"}}),
    )
    .await?;
    cdp.close().await;
    Ok(())
}

pub async fn http_get_text(ctx: &AppContext, path: &str) -> Result<String> {
    let url = format!("http://{}:{}{}", ctx.cdp_host, ctx.cdp_port, path);
    let resp = ctx
        .http
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url} failed"))?;
    timeout(Duration::from_secs(10), resp.text())
        .await
        .with_context(|| format!("GET {url} body read timed out"))?
        .with_context(|| format!("GET {url} body read failed"))
}

pub async fn http_put_text(ctx: &AppContext, path: &str) -> Result<String> {
    let url = format!("http://{}:{}{}", ctx.cdp_host, ctx.cdp_port, path);
    let resp = ctx
        .http
        .put(&url)
        .send()
        .await
        .with_context(|| format!("PUT {url} failed"))?;
    timeout(Duration::from_secs(10), resp.text())
        .await
        .with_context(|| format!("PUT {url} body read timed out"))?
        .with_context(|| format!("PUT {url} body read failed"))
}
