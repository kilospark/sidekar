use super::*;

/// Minimal ISO 8601 date from epoch seconds (no chrono dependency).
fn chrono_lite(epoch_secs: i64) -> String {
    let date_str = epoch_to_date(epoch_secs);
    // epoch_to_date returns "YYYY-MM-DD HH:MM:SS UTC"; convert to "YYYY-MM-DDTHH:MM:SS"
    if date_str.len() >= 19 && date_str.as_bytes()[4] == b'-' {
        format!("{}T{}", &date_str[..10], &date_str[11..19])
    } else {
        date_str
    }
}

pub(crate) async fn cmd_cookies(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let action = args
        .first()
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "get".to_string());
    match action.as_str() {
        "get" => {
            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;
            let result = cdp.send("Network.getCookies", json!({})).await?;
            let cookies = result
                .get("cookies")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if cookies.is_empty() {
                out!(ctx, "No cookies.");
            } else {
                for c in cookies {
                    let name = c.get("name").and_then(Value::as_str).unwrap_or("");
                    let value = c.get("value").and_then(Value::as_str).unwrap_or("");
                    let domain = c.get("domain").and_then(Value::as_str).unwrap_or("");
                    let expires = c.get("expires").and_then(Value::as_f64).unwrap_or(-1.0);
                    let mut flags = Vec::new();
                    if c.get("httpOnly").and_then(Value::as_bool).unwrap_or(false) {
                        flags.push("httpOnly");
                    }
                    if c.get("secure").and_then(Value::as_bool).unwrap_or(false) {
                        flags.push("secure");
                    }
                    if c.get("session").and_then(Value::as_bool).unwrap_or(false) {
                        flags.push("session");
                    }
                    let exp = if expires > 0.0 {
                        format!(" exp:{}", epoch_to_date(expires as i64))
                    } else {
                        String::new()
                    };
                    out!(
                        ctx,
                        "{}={} ({}{} {})",
                        name,
                        truncate(value, 60),
                        domain,
                        exp,
                        flags.join(" ")
                    );
                }
            }
            cdp.close().await;
        }
        "set" => {
            if args.len() < 3 {
                bail!("Usage: sidekar cookies set <name> <value> [domain]");
            }
            let name = args[1].clone();
            let value = args[2].clone();
            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;
            let domain = if args.len() > 3 {
                args[3].clone()
            } else {
                runtime_evaluate(&mut cdp, "location.hostname", true, false)
                    .await?
                    .pointer("/result/value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            cdp.send(
                "Network.setCookie",
                json!({ "name": name, "value": value, "domain": domain, "path": "/" }),
            )
            .await?;
            out!(
                ctx,
                "Cookie set: {}={} ({})",
                name,
                truncate(&value, 40),
                domain
            );
            cdp.close().await;
        }
        "clear" => {
            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;
            cdp.send("Network.clearBrowserCookies", json!({})).await?;
            out!(ctx, "All cookies cleared.");
            cdp.close().await;
        }
        "delete" => {
            if args.len() < 2 {
                bail!("Usage: sidekar cookies delete <name> [domain]");
            }
            let name = args[1].clone();
            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;
            let domain = if args.len() > 2 {
                args[2].clone()
            } else {
                runtime_evaluate(&mut cdp, "location.hostname", true, false)
                    .await?
                    .pointer("/result/value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            cdp.send(
                "Network.deleteCookies",
                json!({ "name": name, "domain": domain }),
            )
            .await?;
            out!(ctx, "Deleted cookie: {} ({})", name, domain);
            cdp.close().await;
        }
        _ => bail!("Usage: sidekar cookies [get|set|clear|delete] [args]"),
    }
    Ok(())
}

pub(crate) async fn cmd_console(ctx: &mut AppContext, action: Option<&str>) -> Result<()> {
    let action = action.unwrap_or("show");
    match action {
        "show" | "errors" => {
            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;
            cdp.send("Runtime.enable", json!({})).await?;
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut logs = Vec::new();
            while Instant::now() < deadline {
                let remain = deadline.saturating_duration_since(Instant::now());
                let Some(event) = cdp.next_event(remain).await? else {
                    break;
                };
                if event.is_null() {
                    continue;
                }
                if event.get("method").and_then(Value::as_str) == Some("Runtime.consoleAPICalled") {
                    let params = event.get("params").cloned().unwrap_or(Value::Null);
                    let event_type = params.get("type").and_then(Value::as_str).unwrap_or("log");
                    if action == "errors" && event_type != "error" {
                        continue;
                    }
                    let args = params
                        .get("args")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let text = args
                        .iter()
                        .map(console_arg_to_text)
                        .collect::<Vec<_>>()
                        .join(" ");
                    logs.push(format!("[{}] {}", event_type, truncate(&text, 200)));
                } else if event.get("method").and_then(Value::as_str)
                    == Some("Runtime.exceptionThrown")
                {
                    let params = event.get("params").cloned().unwrap_or(Value::Null);
                    let desc = params
                        .pointer("/exceptionDetails/exception/description")
                        .and_then(Value::as_str)
                        .or_else(|| {
                            params
                                .pointer("/exceptionDetails/text")
                                .and_then(Value::as_str)
                        })
                        .unwrap_or("Unknown error");
                    logs.push(format!("[exception] {}", truncate(desc, 200)));
                }
            }
            if logs.is_empty() {
                out!(ctx, "No console output captured (listened for 1s).");
            } else {
                out!(ctx, "{}", logs.join("\n"));
            }
            cdp.close().await;
        }
        "listen" => {
            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;
            cdp.send("Runtime.enable", json!({})).await?;
            out!(ctx, "Listening for console output (Ctrl+C to stop)...");
            loop {
                let Some(event) = cdp.next_event(Duration::from_secs(60)).await? else {
                    continue;
                };
                if event.is_null() {
                    continue;
                }
                if event.get("method").and_then(Value::as_str) == Some("Runtime.consoleAPICalled") {
                    let params = event.get("params").cloned().unwrap_or(Value::Null);
                    let event_type = params.get("type").and_then(Value::as_str).unwrap_or("log");
                    let args = params
                        .get("args")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let text = args
                        .iter()
                        .map(console_arg_to_text)
                        .collect::<Vec<_>>()
                        .join(" ");
                    out!(ctx, "[{}] {}", event_type, truncate(&text, 500));
                } else if event.get("method").and_then(Value::as_str)
                    == Some("Runtime.exceptionThrown")
                {
                    let params = event.get("params").cloned().unwrap_or(Value::Null);
                    let desc = params
                        .pointer("/exceptionDetails/exception/description")
                        .and_then(Value::as_str)
                        .or_else(|| {
                            params
                                .pointer("/exceptionDetails/text")
                                .and_then(Value::as_str)
                        })
                        .unwrap_or("Unknown error");
                    out!(ctx, "[exception] {}", truncate(desc, 500));
                }
            }
        }
        _ => bail!("Usage: sidekar console [show|errors|listen]"),
    }
    #[allow(unreachable_code)]
    Ok(())
}

pub(crate) async fn cmd_network(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("capture");
    let log_file = ctx.network_log_file();
    match action {
        "capture" => {
            let duration = args
                .get(1)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(10);
            let filter = args.get(2).cloned();
            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;
            cdp.send("Network.enable", json!({})).await?;
            out!(
                ctx,
                "Capturing network for {}s{}...",
                duration,
                filter
                    .as_ref()
                    .map(|f| format!(" (filter: \"{f}\")"))
                    .unwrap_or_default()
            );

            let mut requests: Vec<NetworkRequestLog> = Vec::new();
            let start = now_epoch_ms();
            let deadline = Instant::now() + Duration::from_secs(duration);
            while Instant::now() < deadline {
                let remain = deadline.saturating_duration_since(Instant::now());
                let Some(event) = cdp.next_event(remain).await? else {
                    break;
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
                        let params = event.get("params").cloned().unwrap_or(Value::Null);
                        let url = params
                            .pointer("/request/url")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        if filter.as_ref().is_some_and(|f| !url.contains(f)) {
                            continue;
                        }
                        let method = params
                            .pointer("/request/method")
                            .and_then(Value::as_str)
                            .unwrap_or("GET")
                            .to_string();
                        let request_id = params
                            .get("requestId")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let req_type = params
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let post_data = params
                            .pointer("/request/postData")
                            .and_then(Value::as_str)
                            .map(|s| truncate(s, 2000));
                        let request_headers = params
                            .pointer("/request/headers")
                            .and_then(Value::as_object)
                            .map(|h| {
                                h.iter()
                                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                                    .collect()
                            });
                        let wall_ms = now_epoch_ms();
                        let started_dt = {
                            let secs = wall_ms / 1000;
                            let ms = wall_ms % 1000;
                            format!("{}.{:03}Z", chrono_lite(secs), ms)
                        };
                        requests.push(NetworkRequestLog {
                            id: request_id,
                            method,
                            url,
                            req_type,
                            time: wall_ms - start,
                            status: None,
                            status_text: None,
                            mime_type: None,
                            post_data,
                            request_headers,
                            response_headers: None,
                            response_size: None,
                            started_date_time: Some(started_dt),
                            time_ms: None,
                        });
                    }
                    "Network.responseReceived" => {
                        let params = event.get("params").cloned().unwrap_or(Value::Null);
                        let request_id = params
                            .get("requestId")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if let Some(req) = requests.iter_mut().find(|r| r.id == request_id) {
                            req.status = params
                                .pointer("/response/status")
                                .and_then(Value::as_f64)
                                .map(|v| v as i64);
                            req.status_text = params
                                .pointer("/response/statusText")
                                .and_then(Value::as_str)
                                .map(ToString::to_string);
                            req.mime_type = params
                                .pointer("/response/mimeType")
                                .and_then(Value::as_str)
                                .map(ToString::to_string);
                            req.response_headers = params
                                .pointer("/response/headers")
                                .and_then(Value::as_object)
                                .map(|h| {
                                    h.iter()
                                        .map(|(k, v)| {
                                            (k.clone(), v.as_str().unwrap_or("").to_string())
                                        })
                                        .collect()
                                });
                            req.response_size = params
                                .pointer("/response/encodedDataLength")
                                .and_then(Value::as_i64);
                            req.time_ms = Some(now_epoch_ms() - start - req.time.max(0) as i64);
                        }
                    }
                    _ => {}
                }
            }

            for r in &requests {
                let status = r
                    .status
                    .map(|s| format!("[{}]", s))
                    .unwrap_or_else(|| "[pending]".to_string());
                out!(
                    ctx,
                    "{} {} {} ({}) +{}ms",
                    r.method,
                    truncate(&r.url, 150),
                    status,
                    if r.req_type.is_empty() {
                        "?"
                    } else {
                        r.req_type.as_str()
                    },
                    r.time
                );
                if let Some(body) = &r.post_data {
                    out!(ctx, "  body: {}", truncate(body, 200));
                }
            }
            out!(ctx, "\n{} requests captured", requests.len());
            fs::write(&log_file, serde_json::to_string_pretty(&requests)?)
                .with_context(|| format!("failed writing {}", log_file.display()))?;
            cdp.close().await;
        }
        "show" => {
            if !log_file.exists() {
                bail!("No captured requests. Run \"network capture\" first.");
            }
            let data = fs::read_to_string(&log_file)
                .with_context(|| format!("failed reading {}", log_file.display()))?;
            let requests: Vec<NetworkRequestLog> = serde_json::from_str(&data)
                .with_context(|| format!("failed parsing {}", log_file.display()))?;
            let filter = args.get(1).cloned();
            let filtered = requests
                .into_iter()
                .filter(|r| filter.as_ref().is_none_or(|f| r.url.contains(f)))
                .collect::<Vec<_>>();
            for r in &filtered {
                let status = r
                    .status
                    .map(|s| format!("[{}]", s))
                    .unwrap_or_else(|| "[pending]".to_string());
                out!(
                    ctx,
                    "{} {} {} ({}) +{}ms",
                    r.method,
                    truncate(&r.url, 150),
                    status,
                    if r.req_type.is_empty() {
                        "?"
                    } else {
                        r.req_type.as_str()
                    },
                    r.time
                );
                if let Some(body) = &r.post_data {
                    out!(ctx, "  body: {}", truncate(body, 200));
                }
            }
            out!(
                ctx,
                "\n{} requests{}",
                filtered.len(),
                filter
                    .as_ref()
                    .map(|f| format!(" matching \"{}\"", f))
                    .unwrap_or_default()
            );
        }
        "har" => {
            if !log_file.exists() {
                bail!("No captured requests. Run \"network capture\" first.");
            }
            let data = fs::read_to_string(&log_file)
                .with_context(|| format!("failed reading {}", log_file.display()))?;
            let requests: Vec<NetworkRequestLog> = serde_json::from_str(&data)
                .with_context(|| format!("failed parsing {}", log_file.display()))?;

            let entries: Vec<Value> = requests
                .iter()
                .map(|r| {
                    let req_headers: Vec<Value> = r
                        .request_headers
                        .as_ref()
                        .map(|h| {
                            h.iter()
                                .map(|(k, v)| json!({"name": k, "value": v}))
                                .collect()
                        })
                        .unwrap_or_default();
                    let resp_headers: Vec<Value> = r
                        .response_headers
                        .as_ref()
                        .map(|h| {
                            h.iter()
                                .map(|(k, v)| json!({"name": k, "value": v}))
                                .collect()
                        })
                        .unwrap_or_default();
                    let wait_ms = r.time_ms.unwrap_or(0);
                    json!({
                        "startedDateTime": r.started_date_time.as_deref().unwrap_or(""),
                        "time": wait_ms,
                        "request": {
                            "method": r.method,
                            "url": r.url,
                            "httpVersion": "HTTP/1.1",
                            "headers": req_headers,
                            "queryString": [],
                            "cookies": [],
                            "headersSize": -1,
                            "bodySize": r.post_data.as_ref().map(|d| d.len() as i64).unwrap_or(-1),
                            "postData": r.post_data.as_ref().map(|d| json!({"mimeType": "application/x-www-form-urlencoded", "text": d})).unwrap_or(Value::Null),
                        },
                        "response": {
                            "status": r.status.unwrap_or(0),
                            "statusText": r.status_text.as_deref().unwrap_or(""),
                            "httpVersion": "HTTP/1.1",
                            "headers": resp_headers,
                            "cookies": [],
                            "content": {
                                "size": r.response_size.unwrap_or(-1),
                                "mimeType": r.mime_type.as_deref().unwrap_or(""),
                            },
                            "redirectURL": "",
                            "headersSize": -1,
                            "bodySize": r.response_size.unwrap_or(-1),
                        },
                        "cache": {},
                        "timings": {
                            "send": 0,
                            "wait": wait_ms,
                            "receive": 0,
                        },
                    })
                })
                .collect();

            let har = json!({
                "log": {
                    "version": "1.2",
                    "creator": {
                        "name": "sidekar",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "entries": entries,
                }
            });

            let output_path = args.get(1).map(String::as_str).unwrap_or("");
            let har_file = if output_path.is_empty() {
                ctx.tmp_dir()
                    .join(format!("sidekar-har-{}.har", ctx.session_id))
            } else {
                std::path::PathBuf::from(output_path)
            };
            fs::write(&har_file, serde_json::to_string_pretty(&har)?)
                .with_context(|| format!("failed writing {}", har_file.display()))?;
            out!(
                ctx,
                "HAR 1.2 exported: {} ({} entries)",
                har_file.display(),
                entries.len()
            );
        }
        _ => bail!("Usage: sidekar network <capture|show|har> [args]"),
    }
    Ok(())
}

pub(crate) async fn cmd_block(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!(
            "Usage: sidekar block <pattern> [pattern2...]\nPatterns: images, css, fonts, media, scripts, or URL substring\nUse \"block off\" to disable blocking."
        );
    }
    let mut state = ctx.load_session_state()?;
    if args.first().map(String::as_str) == Some("off") {
        state.block_patterns = None;
        ctx.save_session_state(&state)?;
        out!(ctx, "Request blocking disabled.");
        return Ok(());
    }

    let mut resource_types = Vec::new();
    let mut url_patterns = Vec::new();

    let has_ads = args
        .iter()
        .any(|p| p == "--ads" || p.eq_ignore_ascii_case("ads"));
    if has_ads {
        url_patterns.extend(ADBLOCK_PATTERNS.iter().map(|p| p.to_string()));
    }
    for p in args {
        if p == "--ads" || p.eq_ignore_ascii_case("ads") {
            continue;
        }
        if let Some(rt) = map_resource_type(p) {
            resource_types.push(rt.to_string());
        } else {
            url_patterns.push(p.clone());
        }
    }

    state.block_patterns = Some(BlockPatterns {
        resource_types,
        url_patterns,
    });
    ctx.save_session_state(&state)?;
    if has_ads {
        out!(
            ctx,
            "Blocking: ads/trackers ({} patterns){}",
            ADBLOCK_PATTERNS.len(),
            if args.len() > 1 {
                format!(" + {}", args.join(", "))
            } else {
                String::new()
            }
        );
    } else {
        out!(
            ctx,
            "Blocking: {}. Takes effect on next page load.",
            args.join(", ")
        );
    }
    Ok(())
}
