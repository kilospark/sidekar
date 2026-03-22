use crate::api_client;
use crate::config;
use crate::*;

use std::io::{self, Write as IoWrite};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::time::{Duration, interval};

const TOOLS_JSON: &str = include_str!("../tools.json");
const MCP_INSTRUCTIONS: &str = include_str!("../MCP_INSTRUCTIONS.md");
const TELEMETRY_INTERVAL: Duration = Duration::from_secs(300); // 5 minutes
const FEEDBACK_DELAY: Duration = Duration::from_secs(1800); // 30 minutes
const DEFAULT_TOOL_TIMEOUT_MS: u64 = 90_000;

/// Tool categories for lazy loading. Core tools are always available.
/// Extended categories can be loaded on demand via the `tools` meta-tool.
const CORE_TOOLS: &[&str] = &[
    "launch", "navigate", "read", "text", "axtree", "observe", "screenshot",
    "click", "type", "fill", "press", "scroll", "keyboard",
    "tabs", "tab", "newtab", "close",
    "search", "readurls",
    "batch", "tools",
    // Bus tools always available
    "who", "bus_send", "bus_done", "register", "unregister",
];

/// Category name → tool names
fn category_tools(cat: &str) -> Option<&'static [&'static str]> {
    match cat {
        "forms" => Some(&["select", "upload", "drag", "clear", "focus", "dialog", "paste", "clipboard", "inserttext"]),
        "nav" => Some(&["back", "forward", "reload", "waitfor", "waitfornav", "find", "resolve"]),
        "debug" => Some(&["console", "network", "block", "eval", "dom", "storage", "cookies", "sw", "security"]),
        "media" => Some(&["viewport", "zoom", "grid", "media", "animations", "pdf", "download"]),
        "desktop" => Some(&["desktop_screenshot", "desktop_apps", "desktop_windows", "desktop_find", "desktop_click", "desktop_launch", "desktop_activate", "desktop_quit"]),
        "session" => Some(&["hover", "lock", "unlock", "activate", "minimize", "kill", "monitor", "frames", "frame"]),
        "meta" => Some(&["feedback", "config", "install"]),
        "all" => None, // special: loads everything
        _ => None,
    }
}

const ALL_CATEGORIES: &[&str] = &["forms", "nav", "debug", "media", "desktop", "session", "meta"];

pub async fn run_mcp_server() -> Result<()> {
    let async_stdin = BufReader::new(tokio::io::stdin());
    let stdout = io::stdout();

    let mut ctx = AppContext::new()?;
    ctx.mcp_mode = true;

    // Try to pick up an existing session
    if let Some(port) = env::var("CDP_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
    {
        ctx.cdp_port = port;
    }

    // Auto-register on the sidekar bus and start IPC socket
    let mut bus_state = bus::SidekarBusState::new();
    bus_state.do_register(None);

    let ipc_socket_path = if let (Some(name), Some(pane_display), Some(session), Some(unique_id)) =
        (bus_state.name.as_deref(), bus_state.pane.as_deref(), bus_state.channel.as_deref(), bus_state.pane_unique_id.as_deref())
    {
        match ipc::start_socket_listener(
            unique_id,
            pane_display,
            session,
            name,
            bus_state.nick.as_deref(),
        ) {
            Ok(path) => Some(path),
            Err(e) => {
                eprintln!("sidekar ipc: socket failed: {e}");
                None
            }
        }
    } else {
        None
    };

    let cfg = config::load_config();
    let mut telemetry_timer = interval(TELEMETRY_INTERVAL);
    telemetry_timer.tick().await; // consume the immediate first tick
    let mut feedback_interval = interval(FEEDBACK_DELAY);
    feedback_interval.tick().await; // consume the immediate first tick
    let mut feedback_prompted = false;
    let mut feedback_received = false;
    let mut session_tool_count: u64 = 0; // lifetime count, independent of telemetry clearing
    let mut active_categories: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut lines = async_stdin.lines();

    loop {
        tokio::select! {
            result = lines.next_line() => {
                let line = match result {
                    Ok(Some(l)) => l,
                    Ok(None) => break,    // EOF — stdin closed
                    Err(e) => {
                        eprintln!("stdin read error: {e}");
                        break;
                    }
                };
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }

                let request: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("Invalid JSON-RPC: {e}");
                        continue;
                    }
                };

                let id = request.get("id").cloned();
                let method = request
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();

                // Process the request. If writing the response fails (stdout closed),
                // break out of the loop so we still send final telemetry.
                let write_err = match method.as_str() {
                    "initialize" => {
                        let current_version = env!("CARGO_PKG_VERSION");
                        // Echo back the client's protocol version for compatibility
                        let client_protocol = request
                            .pointer("/params/protocolVersion")
                            .and_then(Value::as_str)
                            .unwrap_or("2024-11-05");
                        // Background auto-update check+download (throttled to once per 24h).
                        // Entire flow runs in a background task to avoid blocking initialize.
                        if api_client::should_check_for_update() && cfg.auto_update {
                            tokio::spawn(async move {
                                match api_client::check_for_update().await {
                                    Ok(Some(latest)) => {
                                        if let Err(e) = api_client::self_update(&latest).await {
                                            eprintln!("Auto-update failed: {e}");
                                        } else {
                                            eprintln!("Auto-updated to v{latest}. Restart MCP client to use.");
                                        }
                                    }
                                    Ok(None) => {}
                                    Err(e) => eprintln!("Update check failed: {e}"),
                                }
                            });
                        }
                        let instructions = if let (Some(name), Some(channel)) = (bus_state.name.as_deref(), bus_state.channel.as_deref()) {
                            let nick_line = bus_state.nick.as_deref()
                                .map(|n| format!(" Your nickname is **{n}**. Others can use either \"{name}\" or \"{n}\" to reach you."))
                                .unwrap_or_default();
                            format!(
                                "{MCP_INSTRUCTIONS}\n\n## Your Identity\n\n\
                                 You are registered as **{name}** on channel **{channel}**.{nick_line} \
                                 You do not need to register — it happened automatically.\n"
                            )
                        } else {
                            MCP_INSTRUCTIONS.to_string()
                        };

                        let response = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "protocolVersion": client_protocol,
                                "capabilities": {
                                    "tools": {}
                                },
                                "serverInfo": {
                                    "name": "sidekar",
                                    "version": current_version
                                },
                                "instructions": instructions
                            }
                        });
                        write_response(&stdout, &response).err()
                    }
                    "notifications/initialized" => {
                        // No response needed for notifications
                        None
                    }
                    "tools/list" => {
                        let tools = match build_mcp_tools(&active_categories) {
                            Ok(v) => v,
                            Err(e) => {
                                eprintln!("failed building MCP tools list: {e}");
                                break;
                            }
                        };
                        let response = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "tools": tools
                            }
                        });
                        write_response(&stdout, &response).err()
                    }
                    "tools/call" => {
                        let params = request
                            .get("params")
                            .cloned()
                            .unwrap_or(Value::Null);
                        let tool_name = params
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let arguments = params
                            .get("arguments")
                            .cloned()
                            .unwrap_or(json!({}));

                        // Count tool usage for telemetry
                        *ctx.tool_counts.entry(tool_name.clone()).or_insert(0) += 1;
                        session_tool_count += 1;

                        // Handle the `tools` meta-tool inline (needs active_categories + stdout for notification)
                        if tool_name == "tools" {
                            let action = arguments.get("action").and_then(Value::as_str).unwrap_or("list");
                            let result_text = match action {
                                "list" => {
                                    let mut lines = Vec::new();
                                    for cat in ALL_CATEGORIES {
                                        let loaded = if active_categories.contains(*cat) { " [loaded]" } else { "" };
                                        let tool_names = category_tools(cat)
                                            .map(|t| t.join(", "))
                                            .unwrap_or_default();
                                        lines.push(format!("  {cat}{loaded}: {tool_names}"));
                                    }
                                    let all_loaded = active_categories.contains("all");
                                    format!("Tool categories{}:\n{}\n\nUse tools(action: \"load\", category: \"<name>\") to load a category. Use \"all\" to load everything.",
                                        if all_loaded { " [all loaded]" } else { "" },
                                        lines.join("\n"))
                                }
                                "load" => {
                                    let cat = arguments.get("category").and_then(Value::as_str).unwrap_or("");
                                    if cat == "all" {
                                        active_categories.insert("all".to_string());
                                        // Send tools/list_changed notification
                                        let notification = json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"});
                                        let _ = write_response(&stdout, &notification);
                                        "All tool categories loaded.".to_string()
                                    } else if category_tools(cat).is_some() {
                                        active_categories.insert(cat.to_string());
                                        let notification = json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"});
                                        let _ = write_response(&stdout, &notification);
                                        let tool_names = category_tools(cat).unwrap().join(", ");
                                        format!("Loaded category \"{cat}\": {tool_names}")
                                    } else {
                                        format!("Unknown category: \"{cat}\". Available: {}", ALL_CATEGORIES.join(", "))
                                    }
                                }
                                _ => format!("Unknown action: \"{action}\". Use \"list\" or \"load\"."),
                            };
                            let response = json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": { "content": [{"type": "text", "text": result_text}] }
                            });
                            if write_response(&stdout, &response).is_err() { break; }
                            continue;
                        }

                        let result = match tool_call_timeout_ms(&arguments) {
                            Ok(timeout_ms) => match timeout_for_tool_call(timeout_ms) {
                                Some(limit) => match timeout(
                                    limit,
                                    handle_tool_call(&mut ctx, &mut bus_state, &tool_name, &arguments),
                                )
                                .await
                                {
                                    Ok(result) => result,
                                    Err(_) => {
                                        ctx.output.clear();
                                        Err(anyhow!(
                                            "MCP tool `{}` timed out after {}ms. Retry with a larger `timeout_ms`, or set `timeout_ms: 0` to disable the per-call timeout.",
                                            tool_name,
                                            timeout_ms
                                        ))
                                    }
                                },
                                None => handle_tool_call(&mut ctx, &mut bus_state, &tool_name, &arguments).await,
                            },
                            Err(e) => Err(e),
                        };

                        // Mark feedback received only after successful submission
                        if tool_name == "feedback" && result.is_ok() {
                            feedback_received = true;
                            feedback_prompted = false;
                            feedback_interval.reset();
                        }

                        // Check for pending messages and outbound timeouts
                        let mut extra_warnings = Vec::new();
                        if let Some(pane) = &bus_state.pane {
                            if let Some(w) = bus::pending_warnings(pane) {
                                extra_warnings.push(w);
                            }
                        }
                        if let Some(w) = bus::check_outbound_timeouts(&mut bus_state.outbound) {
                            extra_warnings.push(w);
                        }

                        // Build response, appending feedback prompt to both success and error
                        let response = match result {
                            Ok(mut content) => {
                                for warning in &extra_warnings {
                                    content.push(json!({"type": "text", "text": warning}));
                                }
                                if feedback_prompted && !feedback_received && tool_name != "feedback" {
                                    content.push(json!({
                                        "type": "text",
                                        "text": "\n---\n**[sidekar feedback request — low priority]** You've been using sidekar for a while. When you have a moment (and only if the user is not waiting on you), please call the `feedback` tool with a rating (1-5) and a comment focused on improvements. Cover: which tools failed or behaved unexpectedly (tool names, error messages, selectors that didn't match), workarounds you had to use, and what features or fixes would have made the task easier. Skip listing what worked fine — focus on what sidekar should improve. **This is low priority — if the user asks you anything, drop this and attend to them immediately.**"
                                    }));
                                    feedback_prompted = false;
                                }
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {
                                        "content": content
                                    }
                                })
                            }
                            Err(e) => {
                                let mut content = vec![json!({
                                    "type": "text",
                                    "text": format!("Error: {e:#}")
                                })];
                                if feedback_prompted && !feedback_received && tool_name != "feedback" {
                                    content.push(json!({
                                        "type": "text",
                                        "text": "\n---\n**[sidekar feedback request — low priority]** You've been using sidekar for a while. When you have a moment (and only if the user is not waiting on you), please call the `feedback` tool with a rating (1-5) and a comment focused on improvements. Cover: which tools failed or behaved unexpectedly (tool names, error messages, selectors that didn't match), workarounds you had to use, and what features or fixes would have made the task easier. Skip listing what worked fine — focus on what sidekar should improve. **This is low priority — if the user asks you anything, drop this and attend to them immediately.**"
                                    }));
                                    feedback_prompted = false;
                                }
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {
                                        "content": content,
                                        "isError": true
                                    }
                                })
                            }
                        };
                        write_response(&stdout, &response).err()
                    }
                    _ => {
                        // Unknown method -- return error if it has an id
                        if let Some(id) = id {
                            let response = json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": {
                                    "code": -32601,
                                    "message": format!("Method not found: {method}")
                                }
                            });
                            write_response(&stdout, &response).err()
                        } else {
                            None
                        }
                    }
                };

                // If stdout write failed, the host is gone
                if let Some(e) = write_err {
                    eprintln!("stdout write error: {e}");
                    break;
                }
            }
            _ = feedback_interval.tick(), if cfg.feedback => {
                // Every 30 minutes, flag that we should ask for feedback on next tool response
                if !feedback_received && !feedback_prompted && session_tool_count >= 5 {
                    feedback_prompted = true;
                    eprintln!("Feedback prompt queued (session {}s, {} tools used)", ctx.session_start.elapsed().as_secs(), session_tool_count);
                }
            }
            _ = telemetry_timer.tick() => {
                // Periodic telemetry flush every 5 minutes
                if cfg.telemetry && !ctx.tool_counts.is_empty() {
                    let duration = ctx.session_start.elapsed().as_secs();
                    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
                    eprintln!("Periodic telemetry ({} tools, {}s)...", ctx.tool_counts.len(), duration);
                    match api_client::send_telemetry(
                        &ctx.session_id,
                        env!("CARGO_PKG_VERSION"),
                        &platform,
                        duration,
                        &ctx.tool_counts,
                    )
                    .await
                    {
                        Ok(()) => {
                            eprintln!("Periodic telemetry sent.");
                            ctx.tool_counts.clear();
                        }
                        Err(e) => eprintln!("Periodic telemetry failed: {e}"),
                    }
                }
            }
        }
    }

    // Note: shutdown feedback via notifications/message is unreliable (host may have
    // closed the transport) and notifications are one-way (no response path). The
    // recurring in-response prompt is the primary feedback mechanism.

    // Send final telemetry on shutdown
    if cfg.telemetry && !ctx.tool_counts.is_empty() {
        let duration = ctx.session_start.elapsed().as_secs();
        let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        eprintln!(
            "Final telemetry ({} tools, {}s)...",
            ctx.tool_counts.len(),
            duration
        );
        match api_client::send_telemetry(
            &ctx.session_id,
            env!("CARGO_PKG_VERSION"),
            &platform,
            duration,
            &ctx.tool_counts,
        )
        .await
        {
            Ok(()) => eprintln!("Final telemetry sent."),
            Err(e) => eprintln!("Final telemetry failed: {e}"),
        }
    }

    // Clean up IPC socket
    if let Some(path) = &ipc_socket_path {
        let _ = std::fs::remove_file(path);
        eprintln!("sidekar ipc: socket removed");
    }

    Ok(())
}

fn build_mcp_tools(active_categories: &std::collections::HashSet<String>) -> Result<Value> {
    let mut tools: Value =
        serde_json::from_str(TOOLS_JSON).context("failed parsing embedded tools.json")?;

    // Filter tools by active categories (unless "all" is loaded)
    if !active_categories.contains("all") {
        // Build set of allowed tool names
        let mut allowed: std::collections::HashSet<&str> = CORE_TOOLS.iter().copied().collect();
        for cat in active_categories {
            if let Some(cat_tools) = category_tools(cat) {
                allowed.extend(cat_tools.iter());
            }
        }

        let entries = tools.as_array_mut().context("tools.json must be array")?;
        entries.retain(|t| {
            t.get("name")
                .and_then(Value::as_str)
                .map(|n| allowed.contains(n))
                .unwrap_or(false)
        });
    }

    inject_common_timeout_property(&mut tools)?;
    Ok(tools)
}

fn inject_common_timeout_property(tools: &mut Value) -> Result<()> {
    let entries = tools
        .as_array_mut()
        .context("embedded tools.json root must be an array")?;

    for tool in entries {
        let schema = tool
            .get_mut("inputSchema")
            .and_then(Value::as_object_mut)
            .context("tool missing inputSchema object")?;
        let properties = schema
            .entry("properties")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .context("tool inputSchema.properties must be an object")?;
        properties.insert(
            "timeout_ms".to_string(),
            json!({
                "type": "integer",
                "description": format!(
                    "Abort this MCP tool call after this many milliseconds. Default: {}. Use 0 to disable the per-call timeout.",
                    DEFAULT_TOOL_TIMEOUT_MS
                )
            }),
        );
    }

    Ok(())
}

fn write_response(stdout: &io::Stdout, response: &Value) -> Result<()> {
    let mut out = stdout.lock();
    serde_json::to_writer(&mut out, response).context("failed writing JSON-RPC response")?;
    out.write_all(b"\n").context("failed writing newline")?;
    out.flush().context("failed flushing stdout")?;
    Ok(())
}

async fn handle_tool_call(
    ctx: &mut AppContext,
    bus_state: &mut bus::SidekarBusState,
    tool_name: &str,
    arguments: &Value,
) -> Result<Vec<Value>> {
    // Commands that don't need a browser session
    let no_browser = matches!(
        tool_name,
        "launch" | "connect" | "feedback" | "config" | "kill" | "install" | "uninstall"
        | "tools"
        | "who" | "bus_send" | "bus_done" | "register" | "unregister"
        | "desktop_screenshot" | "desktop_apps" | "desktop_windows" | "desktop_find" | "desktop_click"
        | "desktop_launch" | "desktop_activate" | "desktop_quit"
    );

    // Auto-discover or create an isolated session for this MCP process.
    // Each MCP server gets its own session+tab so multiple agents don't collide.
    if !no_browser && ctx.current_session_id.is_none() {
        // Try to get Chrome connection info from the last session
        let chrome_reachable = if ctx.auto_discover_last_session().is_ok() {
            // We found a session — grab its port/host but we'll create our own session
            get_debug_tabs(ctx).await.is_ok()
        } else {
            false
        };

        if chrome_reachable {
            // Chrome is running — create our own isolated session with a fresh window
            eprintln!("Creating isolated session for this agent...");
            ctx.current_session_id = None; // Clear so connect creates a new one
            if ctx.launch_browser_name.is_none() {
                ctx.launch_browser_name = crate::detect_browser_from_port(ctx).await;
            }
            ctx.output.clear();
            commands::dispatch(ctx, "connect", &[]).await?;
            let connect_output = ctx.drain_output();
            eprintln!("Session created: {}", connect_output.trim());
        } else {
            // No Chrome running — launch it (which also creates a session)
            eprintln!("Auto-launching browser for {tool_name}...");
            ctx.output.clear();
            commands::dispatch(ctx, "launch", &[]).await?;
            let launch_output = ctx.drain_output();
            eprintln!("Auto-launch complete: {}", launch_output.trim());
        }
    }

    // Handle bus tools directly (they need bus_state, not the command dispatch)
    match tool_name {
        "who" => {
            let show_all = arguments.get("all").and_then(Value::as_bool).unwrap_or(false);
            bus::cmd_who(bus_state, ctx, show_all)?;
            let output = ctx.drain_output();
            return Ok(vec![json!({"type": "text", "text": output.trim_end()})]);
        }
        "bus_send" => {
            let to = arguments.get("to").and_then(Value::as_str).unwrap_or_default();
            let message = arguments.get("message").and_then(Value::as_str).unwrap_or_default();
            let kind = arguments.get("kind").and_then(Value::as_str).unwrap_or("fyi");
            let reply_to = arguments.get("reply_to").and_then(Value::as_str);
            bus::cmd_send_message(bus_state, ctx, to, message, kind, reply_to)?;
            let output = ctx.drain_output();
            return Ok(vec![json!({"type": "text", "text": output.trim_end()})]);
        }
        "bus_done" => {
            let next = arguments.get("next").and_then(Value::as_str).unwrap_or_default();
            let summary = arguments.get("summary").and_then(Value::as_str).unwrap_or_default();
            let request = arguments.get("request").and_then(Value::as_str).unwrap_or_default();
            let reply_to = arguments.get("reply_to").and_then(Value::as_str);
            bus::cmd_signal_done(bus_state, ctx, next, summary, request, reply_to)?;
            let output = ctx.drain_output();
            return Ok(vec![json!({"type": "text", "text": output.trim_end()})]);
        }
        "register" => {
            let custom_name = arguments.get("name").and_then(Value::as_str);
            bus::cmd_register(bus_state, ctx, custom_name)?;
            let output = ctx.drain_output();
            return Ok(vec![json!({"type": "text", "text": output.trim_end()})]);
        }
        "unregister" => {
            bus::cmd_unregister(bus_state, ctx)?;
            let output = ctx.drain_output();
            return Ok(vec![json!({"type": "text", "text": output.trim_end()})]);
        }
        _ => {}
    }

    // Map tool arguments to CLI args vector
    let args = map_tool_args(tool_name, arguments);

    // Mark tool action timestamp for monitor source attribution
    if tool_name != "monitor" {
        commands::monitor::mark_tool_action();
    }

    // Dispatch the command — on connection failure, reset session so next call auto-recovers
    if let Err(e) = commands::dispatch(ctx, tool_name, &args).await {
        let msg = format!("{e:#}");
        if is_connection_error(&msg) {
            eprintln!("Connection lost — clearing session for auto-recovery on next call");
            ctx.current_session_id = None;
        }
        return Err(e);
    }

    // Drain the output buffer
    let output = ctx.drain_output();

    // Special handling for screenshot: return image content (unless saved to custom path)
    if tool_name == "screenshot" {
        let has_output_path = arguments
            .get("output")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        if !has_output_path {
            return handle_screenshot_output(&output);
        }
    }

    // Special handling for batch: extract inline screenshots from results
    if tool_name == "batch" {
        return handle_batch_output(&output, arguments);
    }

    // Return text content
    let text = output.trim_end().to_string();
    if text.is_empty() {
        Ok(vec![
            json!({ "type": "text", "text": format!("{tool_name}: no output") }),
        ])
    } else {
        Ok(vec![json!({ "type": "text", "text": text })])
    }
}

fn handle_screenshot_output(output: &str) -> Result<Vec<Value>> {
    let path = output
        .lines()
        .find_map(|line| line.trim().strip_prefix("Screenshot saved to "))
        .map(|s| s.trim())
        .unwrap_or_default();

    if path.is_empty() || !std::path::Path::new(path).exists() {
        return Ok(vec![json!({
            "type": "text",
            "text": output.trim_end()
        })]);
    }

    let bytes =
        fs::read(path).with_context(|| format!("failed reading screenshot file: {path}"))?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let mime = if path.ends_with(".png") {
        "image/png"
    } else {
        "image/jpeg"
    };

    Ok(vec![
        json!({
            "type": "image",
            "data": b64,
            "mimeType": mime
        }),
        json!({
            "type": "text",
            "text": output.trim_end()
        }),
    ])
}

fn handle_batch_output(output: &str, arguments: &Value) -> Result<Vec<Value>> {
    let batch_json: Value = match serde_json::from_str(output.trim()) {
        Ok(v) => v,
        Err(_) => {
            // Not valid JSON — return as plain text
            return Ok(vec![json!({ "type": "text", "text": output.trim_end() })]);
        }
    };

    // Collect which action indices are screenshots without an output path
    let actions = arguments
        .get("actions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let inline_screenshot_indices: std::collections::HashSet<usize> = actions
        .iter()
        .enumerate()
        .filter(|(_, a)| {
            a.get("tool").and_then(Value::as_str) == Some("screenshot")
                && !a
                    .get("output")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
        })
        .map(|(i, _)| i)
        .collect();

    if inline_screenshot_indices.is_empty() {
        // No inline screenshots — return batch JSON as-is
        return Ok(vec![json!({ "type": "text", "text": output.trim_end() })]);
    }

    // Extract screenshots from results, replace their output with a marker
    let mut modified_batch = batch_json.clone();
    let mut images: Vec<Value> = Vec::new();

    if let Some(results) = modified_batch.get_mut("results").and_then(Value::as_array_mut) {
        for (i, result) in results.iter_mut().enumerate() {
            if !inline_screenshot_indices.contains(&i) {
                continue;
            }
            if let Some(out_text) = result.get("output").and_then(Value::as_str) {
                let img_result = handle_screenshot_output(out_text);
                if let Ok(content_blocks) = img_result {
                    for block in &content_blocks {
                        if block.get("type").and_then(Value::as_str) == Some("image") {
                            let mut img = block.clone();
                            // Tag the image with the step index for correlation
                            img.as_object_mut()
                                .map(|o| o.insert("_step".to_string(), json!(i)));
                            images.push(img);
                        }
                    }
                }
                // Mark in batch JSON that screenshot was returned inline
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("screenshot_inline".to_string(), json!(true));
                    obj.remove("output");
                }
            }
        }
    }

    let mut content = vec![json!({
        "type": "text",
        "text": serde_json::to_string_pretty(&modified_batch).unwrap_or_else(|_| output.trim_end().to_string())
    })];
    content.extend(images);
    Ok(content)
}

/// Check if an error message indicates a lost CDP/Chrome connection.
fn is_connection_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("websocket closed")
        || lower.contains("closing handshake")
        || lower.contains("tcp connect error")
        || lower.contains("broken pipe")
        || lower.contains("failed to connect cdp")
        || lower.contains("timed out after")
        || lower.contains("eof while parsing")
        || lower.contains("connection aborted")
}

fn tool_call_timeout_ms(arguments: &Value) -> Result<u64> {
    match arguments.get("timeout_ms") {
        None => Ok(DEFAULT_TOOL_TIMEOUT_MS),
        Some(value) => value
            .as_u64()
            .ok_or_else(|| anyhow!("`timeout_ms` must be a non-negative integer")),
    }
}

fn timeout_for_tool_call(timeout_ms: u64) -> Option<Duration> {
    if timeout_ms == 0 {
        None
    } else {
        Some(Duration::from_millis(timeout_ms))
    }
}

fn map_tool_args(command: &str, arguments: &Value) -> Vec<String> {
    match command {
        // Single URL arg
        "navigate" => {
            let mut args = vec_from_opt_str(arguments, "url");
            if arguments
                .get("no_dismiss")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                args.push("--no-dismiss".to_string());
            }
            args
        }
        // Read: optional selector, optional --tokens=N
        "read" | "text" => {
            let mut args = Vec::new();
            if let Some(sel) = arguments.get("selector").and_then(Value::as_str) {
                if !sel.is_empty() {
                    args.push(sel.to_string());
                }
            }
            if let Some(tokens) = arguments.get("max_tokens").and_then(Value::as_i64) {
                if tokens > 0 {
                    args.push(format!("--tokens={tokens}"));
                }
            }
            args
        }
        // DOM: optional selector, optional --tokens=N
        "dom" => {
            let mut args = Vec::new();
            if let Some(sel) = arguments.get("selector").and_then(Value::as_str) {
                if !sel.is_empty() {
                    args.push(sel.to_string());
                }
            }
            if let Some(tokens) = arguments.get("max_tokens").and_then(Value::as_i64) {
                if tokens > 0 {
                    args.push(format!("--tokens={tokens}"));
                }
            }
            args
        }
        // Axtree: optional -i, --diff, selector, --tokens=N
        "axtree" => {
            let mut args = Vec::new();
            if arguments
                .get("interactive")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                args.push("-i".to_string());
            }
            if arguments
                .get("diff")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                args.push("--diff".to_string());
            }
            if let Some(sel) = arguments.get("selector").and_then(Value::as_str) {
                if !sel.is_empty() {
                    args.push(sel.to_string());
                }
            }
            if let Some(tokens) = arguments.get("max_tokens").and_then(Value::as_i64) {
                if tokens > 0 {
                    args.push(format!("--tokens={tokens}"));
                }
            }
            args
        }
        // Click variants, hover: split target on whitespace
        "click" | "hover" => {
            let mut args = split_target(arguments);
            // Handle click mode variants
            if let Some(mode) = arguments.get("mode").and_then(Value::as_str) {
                args.insert(0, format!("--mode={mode}"));
            }
            args
        }
        // Type: selector + text + optional human flag
        "type" => {
            let mut args = Vec::new();
            if arguments.get("human").and_then(Value::as_bool).unwrap_or(false) {
                args.push("--human".to_string());
            }
            if let Some(sel) = arguments.get("selector").and_then(Value::as_str) {
                args.push(sel.to_string());
            }
            if let Some(text) = arguments.get("text").and_then(Value::as_str) {
                args.push(text.to_string());
            }
            args
        }
        // Fill: fields object -> alternating selector/value pairs
        "fill" => {
            let mut args = Vec::new();
            if let Some(fields) = arguments.get("fields").and_then(Value::as_object) {
                for (selector, value) in fields {
                    args.push(selector.clone());
                    args.push(value.as_str().unwrap_or_default().to_string());
                }
            }
            args
        }
        // Keyboard/paste/inserttext: text
        "keyboard" | "paste" | "inserttext" => vec_from_opt_str(arguments, "text"),
        // Clipboard: --html <html> [--text <text>]
        "clipboard" => {
            let mut args = Vec::new();
            if let Some(html) = arguments.get("html").and_then(Value::as_str) {
                args.push("--html".to_string());
                args.push(html.to_string());
            }
            if let Some(text) = arguments.get("text").and_then(Value::as_str) {
                args.push("--text".to_string());
                args.push(text.to_string());
            }
            args
        }
        // Press: key
        "press" => vec_from_opt_str(arguments, "key"),
        // Select: selector + values array
        "select" => {
            let mut args = Vec::new();
            if let Some(sel) = arguments.get("selector").and_then(Value::as_str) {
                args.push(sel.to_string());
            }
            if let Some(vals) = arguments.get("values").and_then(Value::as_array) {
                for v in vals {
                    if let Some(s) = v.as_str() {
                        args.push(s.to_string());
                    } else {
                        args.push(v.to_string());
                    }
                }
            }
            args
        }
        // Upload: selector + files array
        "upload" => {
            let mut args = Vec::new();
            if let Some(sel) = arguments.get("selector").and_then(Value::as_str) {
                args.push(sel.to_string());
            }
            if let Some(files) = arguments.get("files").and_then(Value::as_array) {
                for f in files {
                    if let Some(s) = f.as_str() {
                        args.push(s.to_string());
                    }
                }
            }
            args
        }
        // Drag: from + to
        "drag" => {
            let mut args = Vec::new();
            if let Some(from) = arguments.get("from").and_then(Value::as_str) {
                args.push(from.to_string());
            }
            if let Some(to) = arguments.get("to").and_then(Value::as_str) {
                args.push(to.to_string());
            }
            args
        }
        // Scroll: target (split on whitespace) + optional pixels
        "scroll" => {
            let mut args = Vec::new();
            if let Some(target) = arguments.get("target").and_then(Value::as_str) {
                args.extend(target.split_whitespace().map(String::from));
            }
            if let Some(px) = arguments.get("pixels").and_then(Value::as_i64) {
                args.push(px.to_string());
            }
            args
        }
        // Eval: expression
        "eval" => vec_from_opt_str(arguments, "expression"),
        // Dialog: action + optional text
        "dialog" => {
            let mut args = Vec::new();
            if let Some(action) = arguments.get("action").and_then(Value::as_str) {
                args.push(action.to_string());
            }
            if let Some(text) = arguments.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    args.push(text.to_string());
                }
            }
            args
        }
        // Waitfor: selector + optional timeout
        "waitfor" => {
            let mut args = Vec::new();
            if let Some(sel) = arguments.get("selector").and_then(Value::as_str) {
                args.push(sel.to_string());
            }
            if let Some(t) = arguments.get("timeout").and_then(Value::as_i64) {
                args.push(t.to_string());
            }
            args
        }
        // Waitfornav: optional timeout
        "waitfornav" => {
            let mut args = Vec::new();
            if let Some(t) = arguments.get("timeout").and_then(Value::as_i64) {
                args.push(t.to_string());
            }
            args
        }
        // Cookies: action, name, value, domain
        "cookies" => {
            let mut args = Vec::new();
            if let Some(action) = arguments.get("action").and_then(Value::as_str) {
                args.push(action.to_string());
            }
            if let Some(name) = arguments.get("name").and_then(Value::as_str) {
                args.push(name.to_string());
            }
            if let Some(value) = arguments.get("value").and_then(Value::as_str) {
                args.push(value.to_string());
            }
            if let Some(domain) = arguments.get("domain").and_then(Value::as_str) {
                args.push(domain.to_string());
            }
            args
        }
        // Console: optional action
        "console" => {
            let mut args = Vec::new();
            if let Some(action) = arguments.get("action").and_then(Value::as_str) {
                args.push(action.to_string());
            }
            args
        }
        // Network: action, duration, filter
        "network" => {
            let mut args = Vec::new();
            if let Some(action) = arguments.get("action").and_then(Value::as_str) {
                args.push(action.to_string());
            }
            if let Some(dur) = arguments.get("duration").and_then(Value::as_i64) {
                args.push(dur.to_string());
            }
            if let Some(filter) = arguments.get("filter").and_then(Value::as_str) {
                if !filter.is_empty() {
                    args.push(filter.to_string());
                }
            }
            args
        }
        // Block: patterns array
        "block" => {
            let mut args = Vec::new();
            if let Some(patterns) = arguments.get("patterns").and_then(Value::as_array) {
                for p in patterns {
                    if let Some(s) = p.as_str() {
                        args.push(s.to_string());
                    }
                }
            }
            args
        }
        // Viewport: preset_or_width + optional height
        "viewport" => {
            let mut args = Vec::new();
            if let Some(pw) = arguments.get("preset_or_width").and_then(Value::as_str) {
                args.push(pw.to_string());
            }
            if let Some(h) = arguments.get("height").and_then(Value::as_str) {
                args.push(h.to_string());
            }
            args
        }
        // Zoom: level
        "zoom" => vec_from_opt_str(arguments, "level"),
        // Frame: target
        "frame" => vec_from_opt_str(arguments, "target"),
        // Tab: id
        "tab" => vec_from_opt_str(arguments, "id"),
        // Newtab: optional url
        "newtab" => {
            let mut args = Vec::new();
            if let Some(url) = arguments.get("url").and_then(Value::as_str) {
                if !url.is_empty() {
                    args.push(url.to_string());
                }
            }
            args
        }
        // Lock: optional seconds
        "lock" => {
            let mut args = Vec::new();
            if let Some(s) = arguments.get("seconds").and_then(Value::as_i64) {
                args.push(s.to_string());
            }
            args
        }
        // Download: action, path
        "download" => {
            let mut args = Vec::new();
            if let Some(action) = arguments.get("action").and_then(Value::as_str) {
                args.push(action.to_string());
            }
            if let Some(path) = arguments.get("path").and_then(Value::as_str) {
                if !path.is_empty() {
                    args.push(path.to_string());
                }
            }
            args
        }
        // Search: engine, max_tokens, query
        "search" => {
            let mut args = Vec::new();
            if let Some(engine) = arguments.get("engine").and_then(Value::as_str) {
                if !engine.is_empty() {
                    args.push(format!("--engine={engine}"));
                }
            }
            if let Some(tokens) = arguments.get("max_tokens").and_then(Value::as_u64) {
                args.push(format!("--tokens={tokens}"));
            }
            if let Some(query) = arguments.get("query").and_then(Value::as_str) {
                args.push(query.to_string());
            }
            args
        }
        // Readurls: urls array + optional --tokens=N
        "readurls" => {
            let mut args = Vec::new();
            if let Some(tokens) = arguments.get("max_tokens").and_then(Value::as_u64) {
                args.push(format!("--tokens={tokens}"));
            }
            if let Some(urls) = arguments.get("urls").and_then(Value::as_array) {
                for url in urls {
                    if let Some(u) = url.as_str() {
                        args.push(u.to_string());
                    }
                }
            }
            args
        }
        // Media: features array
        "media" => {
            if let Some(features) = arguments.get("features").and_then(Value::as_array) {
                features
                    .iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            } else {
                vec!["reset".to_string()]
            }
        }
        // Animations: action
        "animations" => vec_from_opt_str(arguments, "action"),
        // Security: action
        "security" => vec_from_opt_str(arguments, "action"),
        // Storage: action, key, value, target, session flag
        "storage" => {
            let mut args = Vec::new();
            if let Some(action) = arguments.get("action").and_then(Value::as_str) {
                args.push(action.to_string());
            }
            if let Some(key) = arguments.get("key").and_then(Value::as_str) {
                if !key.is_empty() {
                    args.push(key.to_string());
                }
            }
            if let Some(value) = arguments.get("value").and_then(Value::as_str) {
                args.push(value.to_string());
            }
            if let Some(target) = arguments.get("target").and_then(Value::as_str) {
                if !target.is_empty() {
                    args.push(target.to_string());
                }
            }
            if arguments
                .get("session")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                args.push("--session".to_string());
            }
            args
        }
        // Service worker: action
        "sw" => vec_from_opt_str(arguments, "action"),
        // Find: query
        "find" => vec_from_opt_str(arguments, "query"),
        // Resolve: selector
        "resolve" => vec_from_opt_str(arguments, "selector"),
        // Pdf: optional path
        "pdf" => {
            let mut args = Vec::new();
            if let Some(path) = arguments.get("path").and_then(Value::as_str) {
                if !path.is_empty() {
                    args.push(path.to_string());
                }
            }
            args
        }
        // Focus/clear: selector
        "focus" | "clear" => vec_from_opt_str(arguments, "selector"),
        "screenshot" => {
            let mut args = Vec::new();
            if let Some(r) = arguments.get("ref").and_then(Value::as_i64) {
                args.push(format!("--ref={r}"));
            } else if let Some(sel) = arguments.get("selector").and_then(Value::as_str) {
                if !sel.is_empty() {
                    args.push(format!("--selector={sel}"));
                }
            }
            if let Some(p) = arguments.get("pad").and_then(Value::as_i64) {
                args.push(format!("--pad={p}"));
            }
            if let Some(fmt) = arguments.get("format").and_then(Value::as_str) {
                args.push(format!("--format={fmt}"));
            }
            if let Some(q) = arguments.get("quality").and_then(Value::as_i64) {
                args.push(format!("--quality={q}"));
            }
            if let Some(s) = arguments.get("scale").and_then(Value::as_f64) {
                args.push(format!("--scale={s}"));
            }
            if arguments
                .get("full")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                args.push("--full".to_string());
            }
            if let Some(o) = arguments.get("output").and_then(Value::as_str) {
                if !o.is_empty() {
                    args.push(format!("--output={o}"));
                }
            }
            args
        }
        // Feedback: rating + optional comment
        "feedback" => {
            let mut args = Vec::new();
            if let Some(r) = arguments.get("rating").and_then(Value::as_i64) {
                args.push(r.to_string());
            }
            if let Some(c) = arguments.get("comment").and_then(Value::as_str) {
                args.push(c.to_string());
            }
            args
        }
        // Config: action + optional key + optional value
        "config" => {
            let mut args = Vec::new();
            if let Some(a) = arguments.get("action").and_then(Value::as_str) {
                args.push(a.to_string());
            }
            if let Some(k) = arguments.get("key").and_then(Value::as_str) {
                args.push(k.to_string());
            }
            if let Some(v) = arguments.get("value") {
                args.push(v.to_string());
            }
            args
        }
        "launch" => {
            let mut args = Vec::new();
            if let Some(browser) = arguments.get("browser").and_then(Value::as_str) {
                if !browser.is_empty() {
                    args.push("--browser".to_string());
                    args.push(browser.to_string());
                }
            }
            if let Some(profile) = arguments.get("profile").and_then(Value::as_str) {
                if !profile.is_empty() {
                    args.push("--profile".to_string());
                    args.push(profile.to_string());
                }
            }
            if arguments
                .get("headless")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                args.push("--headless".to_string());
            }
            args
        }
        // Batch: pass the entire arguments JSON as a single string arg
        "batch" => {
            vec![serde_json::to_string(arguments).unwrap_or_default()]
        }
        // Grid: optional spec
        "grid" => vec_from_opt_str(arguments, "spec"),
        // Monitor: action + optional tabs
        "monitor" => {
            let mut args = Vec::new();
            if let Some(action) = arguments.get("action").and_then(Value::as_str) {
                args.push(action.to_string());
            }
            if let Some(tabs) = arguments.get("tabs").and_then(Value::as_str) {
                // Split comma-separated tab IDs
                for tab in tabs.split(',') {
                    let t = tab.trim();
                    if !t.is_empty() {
                        args.push(t.to_string());
                    }
                }
            }
            args
        }
        // Bus send: to + message
        "bus_send" => {
            let mut args = Vec::new();
            if let Some(to) = arguments.get("to").and_then(Value::as_str) {
                args.push(to.to_string());
            }
            if let Some(msg) = arguments.get("message").and_then(Value::as_str) {
                args.push(msg.to_string());
            }
            args
        }
        // Desktop tools: map MCP underscore names to CLI hyphen names and build args
        "desktop_screenshot" | "desktop_apps" | "desktop_windows" | "desktop_find"
        | "desktop_click" | "desktop_activate" | "desktop_quit" => {
            let mut args = Vec::new();
            if let Some(app) = arguments.get("app").and_then(Value::as_str) {
                if !app.is_empty() {
                    args.push("--app".to_string());
                    args.push(app.to_string());
                }
            }
            if let Some(pid) = arguments.get("pid").and_then(Value::as_i64) {
                args.push("--pid".to_string());
                args.push(pid.to_string());
            }
            if let Some(output) = arguments.get("output").and_then(Value::as_str) {
                if !output.is_empty() {
                    args.push("--output".to_string());
                    args.push(output.to_string());
                }
            }
            if let Some(query) = arguments.get("query").and_then(Value::as_str) {
                if !query.is_empty() {
                    args.push(query.to_string());
                }
            }
            args
        }
        // Desktop launch: takes app name directly
        "desktop_launch" => {
            vec_from_opt_str(arguments, "name")
        }
        // No-arg commands
        "observe" | "frames" | "tabs" | "close" | "back" | "forward" | "reload" | "activate"
        | "minimize" | "unlock" | "kill" | "install" | "who" => Vec::new(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_call_timeout_defaults_to_ninety_seconds() {
        assert_eq!(
            tool_call_timeout_ms(&json!({})).unwrap(),
            DEFAULT_TOOL_TIMEOUT_MS
        );
    }

    #[test]
    fn tool_call_timeout_accepts_zero_as_disabled() {
        assert_eq!(tool_call_timeout_ms(&json!({"timeout_ms": 0})).unwrap(), 0);
        assert_eq!(timeout_for_tool_call(0), None);
    }

    #[test]
    fn tool_call_timeout_rejects_invalid_values() {
        assert!(tool_call_timeout_ms(&json!({"timeout_ms": -1})).is_err());
        assert!(tool_call_timeout_ms(&json!({"timeout_ms": "fast"})).is_err());
    }

    #[test]
    fn batch_output_no_screenshots_returns_text_as_is() {
        let output = r#"{"completed":2,"total":2,"error":null,"results":[{"tool":"click","ok":true,"output":"Clicked","attempts":1},{"tool":"read","ok":true,"output":"Page text","attempts":1}]}"#;
        let args = json!({"actions":[{"tool":"click","target":"--text Foo"},{"tool":"read"}]});
        let result = handle_batch_output(output, &args).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["type"], "text");
    }

    #[test]
    fn batch_output_with_screenshot_with_output_path_returns_text_as_is() {
        let output = r#"{"completed":1,"total":1,"error":null,"results":[{"tool":"screenshot","ok":true,"output":"Screenshot saved to /tmp/test.png","attempts":1}]}"#;
        let args = json!({"actions":[{"tool":"screenshot","output":"/tmp/test.png"}]});
        let result = handle_batch_output(output, &args).unwrap();
        // Has output path → not inline, returns as-is
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["type"], "text");
    }

    #[test]
    fn batch_output_with_inline_screenshot_marks_result() {
        // Screenshot without output path, but file doesn't exist — should still mark inline
        let output = r#"{"completed":2,"total":2,"error":null,"results":[{"tool":"click","ok":true,"output":"Clicked","attempts":1},{"tool":"screenshot","ok":true,"output":"Screenshot saved to /tmp/nonexistent-sidekar-test.png","attempts":1}]}"#;
        let args = json!({"actions":[{"tool":"click","target":"--text Foo"},{"tool":"screenshot"}]});
        let result = handle_batch_output(output, &args).unwrap();
        // File doesn't exist so handle_screenshot_output falls back to text
        // but the batch result should still be processed (screenshot_inline marker)
        assert_eq!(result[0]["type"], "text");
        let batch_json: Value = serde_json::from_str(result[0]["text"].as_str().unwrap()).unwrap();
        let results = batch_json["results"].as_array().unwrap();
        assert_eq!(results[0]["tool"], "click");
        assert!(results[0].get("screenshot_inline").is_none());
        assert_eq!(results[1]["tool"], "screenshot");
        assert_eq!(results[1]["screenshot_inline"], true);
    }

    #[test]
    fn batch_output_invalid_json_returns_plain_text() {
        let output = "not json at all";
        let args = json!({"actions":[]});
        let result = handle_batch_output(output, &args).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["type"], "text");
        assert_eq!(result[0]["text"], "not json at all");
    }

    #[test]
    fn tools_list_includes_common_timeout_property() {
        let active_categories = std::collections::HashSet::new();
        let tools = build_mcp_tools(&active_categories).unwrap();
        let first_tool = tools.as_array().and_then(|v| v.first()).unwrap();
        let timeout_prop = first_tool
            .pointer("/inputSchema/properties/timeout_ms")
            .and_then(Value::as_object)
            .unwrap();
        assert_eq!(
            timeout_prop.get("type").and_then(Value::as_str),
            Some("integer")
        );
    }
}

fn vec_from_opt_str(arguments: &Value, key: &str) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(val) = arguments.get(key).and_then(Value::as_str) {
        if !val.is_empty() {
            args.push(val.to_string());
        }
    }
    args
}

fn split_target(arguments: &Value) -> Vec<String> {
    if let Some(target) = arguments.get("target").and_then(Value::as_str) {
        target.split_whitespace().map(String::from).collect()
    } else {
        Vec::new()
    }
}
