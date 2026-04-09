use super::*;

pub(crate) async fn cmd_media(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;

    if args.is_empty() {
        cdp.send(
            "Emulation.setEmulatedMedia",
            json!({"media": "", "features": []}),
        )
        .await?;
        out!(ctx, "Media emulation reset.");
        cdp.close().await;
        return Ok(());
    }

    let mut media_type = String::new();
    let mut features = Vec::new();

    for arg in args {
        match arg.as_str() {
            "print" => media_type = "print".to_string(),
            "screen" => media_type = "screen".to_string(),
            "dark" => features.push(json!({"name": "prefers-color-scheme", "value": "dark"})),
            "light" => features.push(json!({"name": "prefers-color-scheme", "value": "light"})),
            "reduce-motion" | "no-motion" => {
                features.push(json!({"name": "prefers-reduced-motion", "value": "reduce"}));
            }
            "reduce-transparency" => {
                features.push(json!({"name": "prefers-reduced-transparency", "value": "reduce"}));
            }
            "high-contrast" => {
                features.push(json!({"name": "prefers-contrast", "value": "more"}));
            }
            "reset" => {
                cdp.send(
                    "Emulation.setEmulatedMedia",
                    json!({"media": "", "features": []}),
                )
                .await?;
                out!(ctx, "Media emulation reset.");
                cdp.close().await;
                return Ok(());
            }
            _ => bail!(
                "Unknown media option: {arg}. Valid: dark, light, print, screen, reduce-motion, high-contrast, reset"
            ),
        }
    }

    let mut params = json!({});
    if !media_type.is_empty() {
        params["media"] = json!(media_type);
    }
    if !features.is_empty() {
        params["features"] = json!(features);
    }
    cdp.send("Emulation.setEmulatedMedia", params).await?;

    let mut parts = Vec::new();
    if !media_type.is_empty() {
        parts.push(format!("media={media_type}"));
    }
    for f in &features {
        let name = f["name"].as_str().unwrap_or("");
        let val = f["value"].as_str().unwrap_or("");
        parts.push(format!("{name}={val}"));
    }
    out!(ctx, "Media emulation: {}", parts.join(", "));
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_animations(ctx: &mut AppContext, action: Option<&str>) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;

    match action.unwrap_or("pause") {
        "pause" | "freeze" | "stop" => {
            cdp.send("Animation.enable", json!({})).await?;
            cdp.send("Animation.setPlaybackRate", json!({"playbackRate": 0}))
                .await?;
            out!(ctx, "Animations paused.");
        }
        "resume" | "play" => {
            cdp.send("Animation.enable", json!({})).await?;
            cdp.send("Animation.setPlaybackRate", json!({"playbackRate": 1}))
                .await?;
            out!(ctx, "Animations resumed.");
        }
        "slow" => {
            cdp.send("Animation.enable", json!({})).await?;
            cdp.send("Animation.setPlaybackRate", json!({"playbackRate": 0.1}))
                .await?;
            out!(ctx, "Animations slowed to 10%.");
        }
        other => bail!("Unknown action: {other}. Valid: pause, resume, slow"),
    }

    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_security(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("");
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;

    match action {
        "ignore-certs" | "ignore-cert-errors" => {
            cdp.send(
                "Security.setIgnoreCertificateErrors",
                json!({"ignore": true}),
            )
            .await?;
            out!(ctx, "Certificate errors will be ignored for this session.");
        }
        "strict" | "enforce-certs" => {
            cdp.send(
                "Security.setIgnoreCertificateErrors",
                json!({"ignore": false}),
            )
            .await?;
            out!(ctx, "Certificate validation restored.");
        }
        _ => bail!("Usage: sidekar security <ignore-certs|strict>"),
    }

    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_storage(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("get");
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;

    let origin_result = runtime_evaluate(&mut cdp, "location.origin", true, false).await?;
    let origin = origin_result
        .pointer("/result/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    match action {
        "get" | "list" => {
            let key = args.get(1).map(String::as_str);
            for (label, is_local) in [("localStorage", true), ("sessionStorage", false)] {
                let storage_id = json!({"securityOrigin": origin, "isLocalStorage": is_local});
                let result = cdp
                    .send(
                        "DOMStorage.getDOMStorageItems",
                        json!({"storageId": storage_id}),
                    )
                    .await?;
                let entries = result
                    .get("entries")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if entries.is_empty() {
                    continue;
                }
                let mut lines = vec![format!("{}:", label)];
                for entry in &entries {
                    let arr = entry.as_array();
                    let k = arr
                        .and_then(|a| a.first())
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let v = arr
                        .and_then(|a| a.get(1))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if let Some(filter) = key {
                        if k != filter {
                            continue;
                        }
                    }
                    let display_v = if v.len() > 200 {
                        format!("{}...", &v[..200])
                    } else {
                        v.to_string()
                    };
                    lines.push(format!("  {k} = {display_v}"));
                }
                if lines.len() > 1 {
                    out!(ctx, "{}", lines.join("\n"));
                }
            }
        }
        "set" => {
            let key = args
                .get(1)
                .context("Usage: storage set <key> <value> [--session]")?;
            let value = args.get(2).map(String::as_str).unwrap_or("");
            let is_local = !args.iter().any(|a| a == "--session");
            let storage_id = json!({"securityOrigin": origin, "isLocalStorage": is_local});
            cdp.send(
                "DOMStorage.setDOMStorageItem",
                json!({"storageId": storage_id, "key": key, "value": value}),
            )
            .await?;
            let label = if is_local {
                "localStorage"
            } else {
                "sessionStorage"
            };
            out!(ctx, "Set {label}[{key}] = {value}");
        }
        "remove" | "delete" => {
            let key = args
                .get(1)
                .context("Usage: storage remove <key> [--session]")?;
            let is_local = !args.iter().any(|a| a == "--session");
            let storage_id = json!({"securityOrigin": origin, "isLocalStorage": is_local});
            cdp.send(
                "DOMStorage.removeDOMStorageItem",
                json!({"storageId": storage_id, "key": key}),
            )
            .await?;
            let label = if is_local {
                "localStorage"
            } else {
                "sessionStorage"
            };
            out!(ctx, "Removed {label}[{key}]");
        }
        "clear" => {
            let target = args.get(1).map(String::as_str).unwrap_or("all");
            match target {
                "local" | "localStorage" => {
                    let storage_id = json!({"securityOrigin": origin, "isLocalStorage": true});
                    cdp.send("DOMStorage.clear", json!({"storageId": storage_id}))
                        .await?;
                    out!(ctx, "Cleared localStorage for {origin}");
                }
                "session" | "sessionStorage" => {
                    let storage_id = json!({"securityOrigin": origin, "isLocalStorage": false});
                    cdp.send("DOMStorage.clear", json!({"storageId": storage_id}))
                        .await?;
                    out!(ctx, "Cleared sessionStorage for {origin}");
                }
                "all" => {
                    cdp.send(
                        "Storage.clearDataForOrigin",
                        json!({"origin": origin, "storageTypes": "local_storage,session_storage"}),
                    )
                    .await?;
                    out!(ctx, "Cleared all storage for {origin}");
                }
                "everything" => {
                    cdp.send(
                        "Storage.clearDataForOrigin",
                        json!({"origin": origin, "storageTypes": "all"}),
                    )
                    .await?;
                    out!(
                        ctx,
                        "Cleared all data (storage, cache, cookies, service workers) for {origin}"
                    );
                }
                _ => bail!("Usage: storage clear [local|session|all|everything]"),
            }
        }
        _ => bail!("Usage: storage <get|set|remove|clear> [args]"),
    }

    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_sw(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("list");
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;

    cdp.send("ServiceWorker.enable", json!({})).await?;

    match action {
        "list" | "status" => {
            let origin_result = runtime_evaluate(&mut cdp, "location.origin", true, false).await?;
            let origin = origin_result
                .pointer("/result/value")
                .and_then(Value::as_str)
                .unwrap_or_default();

            let sw_result = runtime_evaluate(
                &mut cdp,
                r#"
                (async () => {
                    const regs = await navigator.serviceWorker.getRegistrations();
                    return regs.map(r => ({
                        scope: r.scope,
                        active: r.active ? r.active.state : null,
                        waiting: r.waiting ? r.waiting.state : null,
                        installing: r.installing ? r.installing.state : null,
                    }));
                })()
                "#,
                true,
                true,
            )
            .await?;
            let regs = sw_result
                .pointer("/result/value")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();

            if regs.is_empty() {
                out!(ctx, "No service workers registered for {origin}");
            } else {
                let mut lines = vec![format!("Service workers for {origin}:")];
                for r in &regs {
                    let scope = r.get("scope").and_then(Value::as_str).unwrap_or("?");
                    let active = r.get("active").and_then(Value::as_str).unwrap_or("none");
                    lines.push(format!("  {scope} — active: {active}"));
                }
                out!(ctx, "{}", lines.join("\n"));
            }
        }
        "unregister" | "remove" | "reset" => {
            let result = runtime_evaluate(
                &mut cdp,
                r#"
                (async () => {
                    const regs = await navigator.serviceWorker.getRegistrations();
                    let count = 0;
                    for (const r of regs) {
                        await r.unregister();
                        count++;
                    }
                    return count;
                })()
                "#,
                true,
                true,
            )
            .await?;
            let count = result
                .pointer("/result/value")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            out!(ctx, "Unregistered {count} service worker(s).");
        }
        "update" => {
            let result = runtime_evaluate(
                &mut cdp,
                r#"
                (async () => {
                    const regs = await navigator.serviceWorker.getRegistrations();
                    for (const r of regs) await r.update();
                    return regs.length;
                })()
                "#,
                true,
                true,
            )
            .await?;
            let count = result
                .pointer("/result/value")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            out!(ctx, "Triggered update for {count} service worker(s).");
        }
        _ => bail!("Usage: sidekar service-workers <list|unregister|update>"),
    }

    cdp.send("ServiceWorker.disable", json!({})).await?;
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_geo(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: geo <lat> <lng> [accuracy]\n       geo off");
    }
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;

    if args.first().map(String::as_str) == Some("off") {
        cdp.send("Emulation.clearGeolocationOverride", json!({}))
            .await?;
        out!(ctx, "Geolocation override cleared");
        cdp.close().await;
        return Ok(());
    }

    let lat: f64 = args
        .first()
        .and_then(|v| v.parse().ok())
        .context("Invalid latitude")?;
    let lng: f64 = args
        .get(1)
        .and_then(|v| v.parse().ok())
        .context("Usage: geo <lat> <lng> [accuracy]")?;
    let accuracy: f64 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(1.0);

    let _ = cdp
        .send(
            "Browser.grantPermissions",
            json!({ "permissions": ["geolocation"] }),
        )
        .await;

    cdp.send(
        "Emulation.setGeolocationOverride",
        json!({ "latitude": lat, "longitude": lng, "accuracy": accuracy }),
    )
    .await?;
    out!(
        ctx,
        "Geolocation set to ({lat}, {lng}) accuracy={accuracy}m"
    );
    cdp.close().await;
    Ok(())
}
