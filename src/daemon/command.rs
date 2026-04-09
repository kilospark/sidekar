use super::*;

pub(super) async fn handle_command(cmd: &Value, state: &Arc<Mutex<DaemonState>>) -> Value {
    let cmd_type = cmd.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match cmd_type {
        "ping" => json!({"pong": true}),

        "status" => {
            let s = state.lock().await;
            let ext_status = crate::ext::get_status(&s.ext_state).await;
            let cli_logged_in = crate::auth::auth_token().is_some();
            json!({
                "running": true,
                "pid": std::process::id(),
                "http_port": s.http_port,
                "ext": ext_status,
                "cli_logged_in": cli_logged_in,
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

            if inner_cmd == "watch" && final_result.is_object() {
                if let Some(dest) = final_result
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
            }

            final_result
        }

        "ext_status" => {
            let s = state.lock().await;
            crate::ext::get_status(&s.ext_state).await
        }

        _ => json!({"error": format!("Unknown command: {cmd_type}")}),
    }
}
