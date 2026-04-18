use super::*;

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

    if let Some(block_patterns) = state.block_patterns.clone() {
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

    if state.stealth_enabled.unwrap_or(false) {
        cdp.send("Page.enable", json!({})).await?;
        let already = state.stealth_script_ids.clone().unwrap_or_default();
        match crate::cdp::stealth::install_on_target(cdp, &already).await {
            Ok(added) => {
                if !added.is_empty() {
                    let mut merged = already;
                    merged.extend(added);
                    state.stealth_script_ids = Some(merged);
                    ctx.save_session_state(&state)?;
                }
            }
            Err(e) => {
                wlog!("stealth script install failed: {e:#}");
            }
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
