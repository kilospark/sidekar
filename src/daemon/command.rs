use super::*;

pub(super) async fn handle_command(cmd: &Value, state: &Arc<Mutex<DaemonState>>) -> Value {
    let cmd_type = cmd.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match cmd_type {
        "ping" => json!({"pong": true, "pid": std::process::id()}),

        "status" => {
            let s = state.lock().await;
            let ext_status = crate::ext::get_status(&s.ext_state).await;
            let cli_logged_in = crate::auth::auth_token().is_some();
            #[cfg(target_os = "macos")]
            let trust = {
                let t = crate::desktop::native::trust_status();
                json!({
                    "accessibility": t.accessibility.as_str(),
                    "screenRecording": t.screen_recording.as_str(),
                    "microphone": t.microphone.as_str(),
                })
            };
            #[cfg(not(target_os = "macos"))]
            let trust = json!(null);
            json!({
                "running": true,
                "pid": std::process::id(),
                "http_port": s.http_port,
                "ext": ext_status,
                "cli_logged_in": cli_logged_in,
                "trust": trust,
            })
        }

        "stop" => {
            tokio::spawn(async {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                let _ = std::fs::remove_file(pid_path());
                let _ = std::fs::remove_file(socket_path());
                std::process::exit(0);
            });
            json!({"ok": true, "message": "Daemon stopping"})
        }

        "ext" => {
            let ext_cmd = cmd.get("command").cloned().unwrap_or(json!({}));
            let agent_id = cmd
                .get("agent_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            let target_conn = cmd.get("conn_id").and_then(|v| v.as_u64());
            let target_profile = cmd
                .get("profile")
                .and_then(|v| v.as_str())
                .map(String::from);
            let deliver_to = cmd
                .get("deliver_to")
                .and_then(|v| v.as_str())
                .map(String::from);

            let inner_cmd = ext_cmd
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let inner_selector = ext_cmd
                .get("selector")
                .and_then(|v| v.as_str())
                .map(String::from);
            let inner_watch_id = ext_cmd
                .get("watchId")
                .and_then(|v| v.as_str())
                .map(String::from);

            let ext_state = {
                let s = state.lock().await;
                s.ext_state.clone()
            };
            let routed = crate::ext::forward_command(
                &ext_state,
                ext_cmd,
                agent_id,
                target_conn,
                target_profile,
            )
            .await;
            let (mut final_result, routed_conn_id, routed_profile) = match routed {
                Ok(routed) => (routed.response, routed.conn_id, routed.profile),
                Err(e) => return json!({"error": e.to_string()}),
            };

            if inner_cmd == "watch" {
                if let (Some(wid), Some(sel), Some(dest)) = (
                    final_result
                        .get("watchId")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    final_result
                        .get("selector")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .or(inner_selector),
                    deliver_to,
                ) {
                    crate::ext::register_watch(
                        &ext_state,
                        wid,
                        sel,
                        dest,
                        routed_conn_id,
                        routed_profile.clone(),
                    )
                    .await;
                }
            } else if inner_cmd == "unwatch" {
                if let Some(wid) = inner_watch_id {
                    crate::ext::remove_watch(&ext_state, &wid).await;
                } else {
                    let mut s = ext_state.lock().await;
                    s.watches.clear();
                }
            }

            if inner_cmd == "watch"
                && final_result.is_object()
                && let Some(dest) = final_result
                    .get("watchId")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            {
                let _ = dest;
                if let Some(obj) = final_result.as_object_mut() {
                    let deliver = {
                        let s = ext_state.lock().await;
                        obj.get("watchId")
                            .and_then(|v| v.as_str())
                            .and_then(|wid| s.watches.get(wid).map(|w| w.deliver_to.clone()))
                    };
                    if let Some(d) = deliver {
                        obj.insert("deliverTo".into(), json!(d));
                    }
                }
            }

            final_result
        }

        "ext_status" => {
            let s = state.lock().await;
            crate::ext::get_status(&s.ext_state).await
        }

        "net_passive" => {
            let action = cmd
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("log")
                .to_string();
            let target_conn = cmd.get("conn_id").and_then(|v| v.as_u64());
            let target_profile = cmd
                .get("profile")
                .and_then(|v| v.as_str())
                .map(String::from);
            let limit = cmd.get("limit").and_then(|v| v.as_u64()).map(|n| n as usize);

            let ext_state = {
                let s = state.lock().await;
                s.ext_state.clone()
            };

            let conn_id = {
                let s = ext_state.lock().await;
                if s.connections.is_empty() {
                    return json!({"error": "Extension not connected"});
                }
                if let Some(cid) = target_conn {
                    if !s.connections.contains_key(&cid) {
                        return json!({"error": format!("Connection {cid} not found")});
                    }
                    cid
                } else if let Some(p) = target_profile.as_deref() {
                    let lp = p.to_lowercase();
                    match s
                        .connections
                        .iter()
                        .find(|(_, c)| c.profile.to_lowercase().contains(&lp))
                        .map(|(id, _)| *id)
                    {
                        Some(cid) => cid,
                        None => {
                            return json!({
                                "error": format!("No connection matching profile '{p}'")
                            });
                        }
                    }
                } else if s.connections.len() == 1 {
                    *s.connections.keys().next().unwrap()
                } else {
                    return json!({
                        "error": "Multiple extensions connected; pass --conn or --profile"
                    });
                }
            };

            match action.as_str() {
                "log" | "tail" => {
                    let events = crate::ext::passive_snapshot(&ext_state, conn_id, limit).await;
                    json!({"ok": true, "events": events, "connId": conn_id})
                }
                "clear" => {
                    let dropped = crate::ext::passive_clear(&ext_state, conn_id).await;
                    json!({"ok": true, "cleared": dropped, "connId": conn_id})
                }
                "stats" => {
                    let mut stats = crate::ext::passive_stats(&ext_state, conn_id).await;
                    if let Some(obj) = stats.as_object_mut() {
                        obj.insert("connId".into(), json!(conn_id));
                    }
                    stats
                }
                "sse_streams" | "sse_list" => {
                    // Group sse/sse_open/sse_done/sse_error by url.
                    let events =
                        crate::ext::passive_snapshot(&ext_state, conn_id, None).await;
                    let mut by_url: std::collections::BTreeMap<String, Value> =
                        std::collections::BTreeMap::new();
                    for e in events {
                        let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                        if !kind.starts_with("sse") {
                            continue;
                        }
                        let url = e
                            .get("detail")
                            .and_then(|v| v.get("url"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if url.is_empty() {
                            continue;
                        }
                        let entry = by_url.entry(url.clone()).or_insert_with(|| {
                            json!({
                                "url": url,
                                "chunks": 0,
                                "bytes": 0,
                                "open": false,
                                "done": false,
                                "error": Value::Null,
                                "firstT": Value::Null,
                                "lastT": Value::Null,
                            })
                        });
                        let detail = e.get("detail").cloned().unwrap_or(Value::Null);
                        let t = detail.get("t").and_then(|v| v.as_i64());
                        if let Some(t) = t {
                            let obj = entry.as_object_mut().unwrap();
                            if obj.get("firstT").map(|v| v.is_null()).unwrap_or(true) {
                                obj.insert("firstT".into(), json!(t));
                            }
                            obj.insert("lastT".into(), json!(t));
                        }
                        match kind {
                            "sse_open" => {
                                entry["open"] = json!(true);
                            }
                            "sse" => {
                                let c = entry["chunks"].as_u64().unwrap_or(0) + 1;
                                entry["chunks"] = json!(c);
                                let chunk_len = detail
                                    .get("chunk")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.len() as u64)
                                    .unwrap_or(0);
                                let b = entry["bytes"].as_u64().unwrap_or(0) + chunk_len;
                                entry["bytes"] = json!(b);
                            }
                            "sse_done" => {
                                entry["done"] = json!(true);
                                if let Some(tc) = detail.get("totalChunks") {
                                    entry["totalChunks"] = tc.clone();
                                }
                                if let Some(tb) = detail.get("totalBytes") {
                                    entry["totalBytes"] = tb.clone();
                                }
                                if let Some(d) = detail.get("duration") {
                                    entry["duration"] = d.clone();
                                }
                                if let Some(t) = detail.get("truncated") {
                                    entry["truncated"] = t.clone();
                                }
                            }
                            "sse_error" => {
                                if let Some(err) = detail.get("err") {
                                    entry["error"] = err.clone();
                                }
                            }
                            _ => {}
                        }
                    }
                    let streams: Vec<Value> = by_url.into_values().collect();
                    json!({"ok": true, "streams": streams, "connId": conn_id})
                }
                "emit_off" | "emit_on" => {
                    let off = action == "emit_off";
                    let bridge_tx = {
                        let s = ext_state.lock().await;
                        s.connections.get(&conn_id).map(|c| c.bridge_tx.clone())
                    };
                    let Some(bridge_tx) = bridge_tx else {
                        return json!({"error": "Extension connection gone"});
                    };
                    let frame = json!({
                        "type": "passive_emit_ctl",
                        "off": off,
                        "id": format!("emit-{}", rand::random::<u32>()),
                    });
                    let mut line = frame.to_string();
                    line.push('\n');
                    let _ = bridge_tx.send(line);
                    json!({"ok": true, "emitOff": off, "connId": conn_id})
                }
                "sse_log" => {
                    let target_url = cmd
                        .get("url")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let events =
                        crate::ext::passive_snapshot(&ext_state, conn_id, None).await;
                    let mut chunks: Vec<Value> = Vec::new();
                    for e in events {
                        let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                        if kind != "sse" {
                            continue;
                        }
                        let url = e
                            .get("detail")
                            .and_then(|v| v.get("url"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if let Some(t) = &target_url
                            && !url.contains(t)
                        {
                            continue;
                        }
                        chunks.push(e.get("detail").cloned().unwrap_or(Value::Null));
                    }
                    json!({"ok": true, "chunks": chunks, "connId": conn_id})
                }
                other => json!({"error": format!("Unknown passive action: {other}")}),
            }
        }

        #[cfg(target_os = "macos")]
        "desktop_monitor" => {
            use crate::desktop::monitor;
            let action = cmd
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("stats");
            match action {
                "start" => match monitor::start() {
                    Ok(()) => {
                        let stats = monitor::stats();
                        json!({"ok": true, "action": "start", "stats": stats})
                    }
                    Err(e) => json!({"error": format!("{e:#}")}),
                },
                "stop" => match monitor::stop() {
                    Ok(()) => json!({"ok": true, "action": "stop"}),
                    Err(e) => json!({"error": format!("{e:#}")}),
                },
                "clear" => {
                    let n = monitor::clear();
                    json!({"ok": true, "cleared": n})
                }
                "stats" => monitor::stats(),
                "log" | "tail" => {
                    let limit = cmd
                        .get("limit")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize);
                    let events: Vec<Value> =
                        monitor::snapshot(limit).iter().map(|e| e.as_json()).collect();
                    json!({"ok": true, "events": events})
                }
                other => json!({"error": format!("Unknown monitor action: {other}")}),
            }
        }

        _ => json!({"error": format!("Unknown command: {cmd_type}")}),
    }
}
