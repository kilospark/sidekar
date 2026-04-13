use super::*;
use crate::output::PlainOutput;

#[derive(serde::Serialize)]
struct CookieEntry {
    name: String,
    value: String,
    domain: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    flags: Vec<String>,
}

#[derive(serde::Serialize)]
struct CookiesOutput {
    cookies: Vec<CookieEntry>,
}

impl crate::output::CommandOutput for CookiesOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.cookies.is_empty() {
            writeln!(w, "No cookies.")?;
            return Ok(());
        }
        for c in &self.cookies {
            let exp = c
                .expires
                .as_ref()
                .map(|e| format!(" exp:{e}"))
                .unwrap_or_default();
            writeln!(
                w,
                "{}={} ({}{} {})",
                c.name,
                c.value,
                c.domain,
                exp,
                c.flags.join(" ")
            )?;
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct ConsoleLogEntry {
    kind: String,
    text: String,
}

#[derive(serde::Serialize)]
struct ConsoleLogsOutput {
    entries: Vec<ConsoleLogEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

impl crate::output::CommandOutput for ConsoleLogsOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if let Some(note) = &self.note {
            writeln!(w, "{note}")?;
            return Ok(());
        }
        for e in &self.entries {
            writeln!(w, "[{}] {}", e.kind, e.text)?;
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct NetworkRow {
    method: String,
    url: String,
    status: Option<i64>,
    req_type: String,
    time_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
}

#[derive(serde::Serialize)]
struct NetworkListOutput {
    prefix: Option<String>,
    rows: Vec<NetworkRow>,
    footer: String,
}

impl crate::output::CommandOutput for NetworkListOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if let Some(prefix) = &self.prefix {
            writeln!(w, "{prefix}")?;
        }
        for r in &self.rows {
            let status = r
                .status
                .map(|s| format!("[{}]", s))
                .unwrap_or_else(|| "[pending]".to_string());
            let req_type = if r.req_type.is_empty() {
                "?"
            } else {
                r.req_type.as_str()
            };
            writeln!(
                w,
                "{} {} {} ({}) +{}ms",
                r.method, r.url, status, req_type, r.time_ms
            )?;
            if let Some(body) = &r.body {
                writeln!(w, "  body: {body}")?;
            }
        }
        writeln!(w)?;
        writeln!(w, "{}", self.footer)?;
        Ok(())
    }
}

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
            let entries: Vec<CookieEntry> = cookies
                .iter()
                .map(|c| {
                    let name = c.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                    let value = truncate(
                        c.get("value").and_then(Value::as_str).unwrap_or(""),
                        60,
                    );
                    let domain = c
                        .get("domain")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let expires = c.get("expires").and_then(Value::as_f64).unwrap_or(-1.0);
                    let mut flags: Vec<String> = Vec::new();
                    if c.get("httpOnly").and_then(Value::as_bool).unwrap_or(false) {
                        flags.push("httpOnly".to_string());
                    }
                    if c.get("secure").and_then(Value::as_bool).unwrap_or(false) {
                        flags.push("secure".to_string());
                    }
                    if c.get("session").and_then(Value::as_bool).unwrap_or(false) {
                        flags.push("session".to_string());
                    }
                    let expires = if expires > 0.0 {
                        Some(epoch_to_date(expires as i64))
                    } else {
                        None
                    };
                    CookieEntry {
                        name,
                        value,
                        domain,
                        expires,
                        flags,
                    }
                })
                .collect();
            let output = CookiesOutput { cookies: entries };
            out!(ctx, "{}", crate::output::to_string(&output)?);
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
            let msg = format!(
                "Cookie set: {}={} ({})",
                name,
                truncate(&value, 40),
                domain
            );
            out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
            cdp.close().await;
        }
        "clear" => {
            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;
            cdp.send("Network.clearBrowserCookies", json!({})).await?;
            out!(ctx, "{}", crate::output::to_string(&PlainOutput::new("All cookies cleared."))?);
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
            let msg = format!("Deleted cookie: {} ({})", name, domain);
            out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
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
            let output = if logs.is_empty() {
                ConsoleLogsOutput {
                    entries: Vec::new(),
                    note: Some("No console output captured (listened for 1s).".to_string()),
                }
            } else {
                let entries = logs
                    .iter()
                    .map(|line| {
                        // Lines look like "[kind] text"; split once.
                        if let Some(rest) = line.strip_prefix('[')
                            && let Some(idx) = rest.find(']')
                        {
                            let kind = rest[..idx].to_string();
                            let text = rest[idx + 1..].trim_start().to_string();
                            return ConsoleLogEntry { kind, text };
                        }
                        ConsoleLogEntry {
                            kind: "log".to_string(),
                            text: line.clone(),
                        }
                    })
                    .collect();
                ConsoleLogsOutput {
                    entries,
                    note: None,
                }
            };
            out!(ctx, "{}", crate::output::to_string(&output)?);
            cdp.close().await;
        }
        "listen" => {
            let mut cdp = open_cdp(ctx).await?;
            prepare_cdp(ctx, &mut cdp).await?;
            cdp.send("Runtime.enable", json!({})).await?;
            out!(ctx, "{}", crate::output::to_string(&PlainOutput::new("Listening for console output (Ctrl+C to stop)..."))?);
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
                    let line = format!("[{}] {}", event_type, truncate(&text, 500));
                    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(line))?);
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
                    let line = format!("[exception] {}", truncate(desc, 500));
                    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(line))?);
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
            let prefix = format!(
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
                            req.time_ms = Some(now_epoch_ms() - start - req.time.max(0));
                        }
                    }
                    _ => {}
                }
            }

            let rows: Vec<NetworkRow> = requests
                .iter()
                .map(|r| NetworkRow {
                    method: r.method.clone(),
                    url: truncate(&r.url, 150),
                    status: r.status,
                    req_type: r.req_type.clone(),
                    time_ms: r.time,
                    body: r.post_data.as_ref().map(|b| truncate(b, 200)),
                })
                .collect();
            let footer = format!("{} requests captured", requests.len());
            let output = NetworkListOutput {
                prefix: Some(prefix),
                rows,
                footer,
            };
            out!(ctx, "{}", crate::output::to_string(&output)?);
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
            let rows: Vec<NetworkRow> = filtered
                .iter()
                .map(|r| NetworkRow {
                    method: r.method.clone(),
                    url: truncate(&r.url, 150),
                    status: r.status,
                    req_type: r.req_type.clone(),
                    time_ms: r.time,
                    body: r.post_data.as_ref().map(|b| truncate(b, 200)),
                })
                .collect();
            let footer = format!(
                "{} requests{}",
                filtered.len(),
                filter
                    .as_ref()
                    .map(|f| format!(" matching \"{}\"", f))
                    .unwrap_or_default()
            );
            let output = NetworkListOutput {
                prefix: None,
                rows,
                footer,
            };
            out!(ctx, "{}", crate::output::to_string(&output)?);
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
            let msg = format!(
                "HAR 1.2 exported: {} ({} entries)",
                har_file.display(),
                entries.len()
            );
            out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
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
        out!(ctx, "{}", crate::output::to_string(&PlainOutput::new("Request blocking disabled."))?);
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
    let msg = if has_ads {
        format!(
            "Blocking: ads/trackers ({} patterns){}",
            ADBLOCK_PATTERNS.len(),
            if args.len() > 1 {
                format!(" + {}", args.join(", "))
            } else {
                String::new()
            }
        )
    } else {
        format!(
            "Blocking: {}. Takes effect on next page load.",
            args.join(", ")
        )
    };
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
    Ok(())
}
