use crate::*;

mod batch;
mod core;
pub mod cron;
mod data;
mod desktop;
mod interaction;
pub mod monitor;
mod session;
pub mod totp;
pub mod kv;

use batch::*;
use core::*;
use data::*;
use desktop::*;
use interaction::*;
use monitor::*;
use session::*;
use totp::*;
use kv::*;

pub async fn dispatch(ctx: &mut AppContext, command: &str, args: &[String]) -> Result<()> {
    match command {
        "launch" => cmd_launch(ctx, args).await,
        "connect" => cmd_connect(ctx).await.map(|_| ()),
        "navigate" => {
            if args.is_empty() {
                bail!("Usage: sidekar navigate <url>");
            }
            let no_dismiss = args.iter().any(|a| a == "--no-dismiss");
            let url_parts: Vec<&str> = args
                .iter()
                .filter(|a| *a != "--no-dismiss")
                .map(String::as_str)
                .collect();
            cmd_navigate(ctx, &url_parts.join(" "), !no_dismiss).await
        }
        "dom" => {
            let mut max_tokens = 0usize;
            let mut selector_parts = Vec::new();
            for arg in args {
                if let Some(raw) = arg.strip_prefix("--tokens=") {
                    max_tokens = raw.parse::<usize>().unwrap_or(0);
                } else if arg != "--full" {
                    selector_parts.push(arg.clone());
                }
            }

            let selector = if selector_parts.is_empty() {
                None
            } else {
                Some(resolve_selector(ctx, &selector_parts.join(" "))?)
            };
            cmd_dom(ctx, selector.as_deref(), max_tokens).await
        }
        "read" => {
            let mut max_tokens = 0usize;
            let mut selector_parts = Vec::new();
            for arg in args {
                if let Some(raw) = arg.strip_prefix("--tokens=") {
                    max_tokens = raw.parse::<usize>().unwrap_or(0);
                } else {
                    selector_parts.push(arg.clone());
                }
            }
            let selector = if selector_parts.is_empty() {
                None
            } else {
                Some(resolve_selector(ctx, &selector_parts.join(" "))?)
            };
            cmd_read(ctx, selector.as_deref(), max_tokens).await
        }
        "text" => {
            let mut max_tokens = 0usize;
            let mut selector_parts = Vec::new();
            for arg in args {
                if let Some(raw) = arg.strip_prefix("--tokens=") {
                    max_tokens = raw.parse::<usize>().unwrap_or(0);
                } else {
                    selector_parts.push(arg.clone());
                }
            }
            let selector = if selector_parts.is_empty() {
                None
            } else {
                Some(resolve_selector(ctx, &selector_parts.join(" "))?)
            };
            cmd_text(ctx, selector.as_deref(), max_tokens).await
        }
        "axtree" => {
            let interactive = args.iter().any(|a| a == "-i" || a == "--interactive");
            let diff = args.iter().any(|a| a == "--diff");
            let selector = args
                .iter()
                .filter(|a| !matches!(a.as_str(), "-i" | "--interactive" | "--diff"))
                .find(|a| !a.starts_with("--tokens="))
                .map(|s| s.as_str());
            let max_tokens = args
                .iter()
                .find_map(|a| a.strip_prefix("--tokens="))
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            if interactive {
                cmd_axtree_interactive(ctx, max_tokens, diff).await
            } else {
                // Default cap for full axtree to prevent 1M+ char output
                let effective_max = if max_tokens > 0 { max_tokens } else { 4000 };
                cmd_axtree_full(ctx, selector, effective_max).await
            }
        }
        "screenshot" => cmd_screenshot(ctx, args).await,
        "pdf" => cmd_pdf(ctx, args.first().map(String::as_str)).await,
        "click" => {
            // Extract --mode=<mode> if present
            let mode = args
                .iter()
                .find_map(|a| a.strip_prefix("--mode="))
                .map(String::from);
            let filtered: Vec<String> = args
                .iter()
                .filter(|a| !a.starts_with("--mode="))
                .cloned()
                .collect();
            if filtered.is_empty() {
                bail!("Usage: sidekar click <sel|x,y|--text> [--mode=double|right|human]");
            }
            match mode.as_deref() {
                Some("double") => cmd_double_click_dispatch(ctx, &filtered).await,
                Some("right") => cmd_right_click_dispatch(ctx, &filtered).await,
                Some("human") => cmd_human_click_dispatch(ctx, &filtered).await,
                None => cmd_click_dispatch(ctx, &filtered).await,
                Some(m) => bail!("Unknown click mode: {m}. Valid: double, right, human"),
            }
        }
        "hover" => {
            if args.is_empty() {
                bail!("Usage: sidekar hover <sel|x,y|--text>");
            }
            cmd_hover_dispatch(ctx, args).await
        }
        "focus" => {
            let selector = resolve_selector(ctx, &args.join(" "))?;
            cmd_focus(ctx, &selector).await
        }
        "clear" => {
            let selector = resolve_selector(ctx, &args.join(" "))?;
            cmd_clear(ctx, &selector).await
        }
        "type" => {
            let human = args.first().map(|a| a == "--human").unwrap_or(false);
            let filtered: Vec<&String> = args.iter().filter(|a| *a != "--human").collect();
            let selector_arg = filtered
                .first()
                .cloned()
                .context("Usage: sidekar type <selector> <text> [--human]")?;
            let text = filtered
                .iter()
                .skip(1)
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if text.is_empty() {
                bail!("Usage: sidekar type <selector> <text> [--human]");
            }
            let selector = resolve_selector(ctx, selector_arg)?;
            if human {
                cmd_human_type(ctx, &selector, &text).await
            } else {
                cmd_type(ctx, &selector, &text).await
            }
        }
        "fill" => {
            let fields: Vec<(String, String)> = args
                .chunks(2)
                .filter(|c| c.len() == 2)
                .map(|c| (c[0].clone(), c[1].clone()))
                .collect();
            cmd_fill(ctx, &fields).await
        }
        "keyboard" => cmd_keyboard(ctx, &args.join(" ")).await,
        "paste" => cmd_paste(ctx, &args.join(" ")).await,
        "clipboard" => {
            let mut html: Option<String> = None;
            let mut text: Option<String> = None;
            let mut i = 0;
            while i < args.len() {
                match args[i].as_str() {
                    "--html" => {
                        i += 1;
                        if i < args.len() {
                            html = Some(args[i..].join(" "));
                            // If --text comes later, split at --text
                            if let Some(pos) = html.as_ref().unwrap().find(" --text ") {
                                let full = html.take().unwrap();
                                html = Some(full[..pos].to_string());
                                text = Some(full[pos + 8..].to_string());
                            }
                            break;
                        }
                    }
                    "--text" => {
                        i += 1;
                        if i < args.len() {
                            text = Some(args[i..].join(" "));
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            cmd_clipboard(ctx, html.as_deref(), text.as_deref()).await
        }
        "inserttext" => cmd_inserttext(ctx, &args.join(" ")).await,
        "select" => {
            let selector = args
                .first()
                .cloned()
                .context("Usage: sidekar select <selector> <value> [value2...]")?;
            let selector = resolve_selector(ctx, &selector)?;
            cmd_select(ctx, &selector, &args[1..]).await
        }
        "upload" => {
            let selector = args
                .first()
                .cloned()
                .context("Usage: sidekar upload <selector> <file> [file2...]")?;
            let selector = resolve_selector(ctx, &selector)?;
            cmd_upload(ctx, &selector, &args[1..]).await
        }
        "drag" => {
            if args.len() < 2 {
                bail!("Usage: sidekar drag <from> <to>");
            }
            let from = resolve_selector(ctx, &args[0])?;
            let to = resolve_selector(ctx, &args[1])?;
            cmd_drag(ctx, &from, &to).await
        }
        "dialog" => cmd_dialog(ctx, args.first().map(String::as_str), &args[1..]).await,
        "waitfor" => {
            let selector = args
                .first()
                .cloned()
                .context("Usage: sidekar waitfor <selector> [timeout_ms]")?;
            let selector = resolve_selector(ctx, &selector)?;
            cmd_wait_for(ctx, &selector, args.get(1).map(String::as_str)).await
        }
        "waitfornav" => cmd_wait_for_nav(ctx, args.first().map(String::as_str)).await,
        "press" => {
            let key = args
                .first()
                .cloned()
                .context("Usage: sidekar press <key>")?;
            cmd_press(ctx, &key).await
        }
        "scroll" => cmd_scroll(ctx, args).await,
        "eval" => cmd_eval(ctx, &args.join(" ")).await,
        "observe" => cmd_observe(ctx).await,
        "find" => cmd_find(ctx, &args.join(" ")).await,
        "resolve" => {
            let selector = resolve_selector(ctx, &args.join(" "))?;
            cmd_resolve(ctx, &selector).await
        }
        "cookies" => cmd_cookies(ctx, args).await,
        "console" => cmd_console(ctx, args.first().map(String::as_str)).await,
        "network" => cmd_network(ctx, args).await,
        "block" => cmd_block(ctx, args).await,
        "viewport" => {
            cmd_viewport(
                ctx,
                args.first().map(String::as_str),
                args.get(1).map(String::as_str),
            )
            .await
        }
        "zoom" => cmd_zoom(ctx, args.first().map(String::as_str)).await,
        "frames" => cmd_frames(ctx).await,
        "frame" => cmd_frame(ctx, args.first().map(String::as_str)).await,
        "download" => cmd_download(ctx, args).await,
        "tabs" => cmd_tabs(ctx).await,
        "tab" => {
            let id = args.first().cloned().context("Usage: sidekar tab <id>")?;
            cmd_tab(ctx, &id).await
        }
        "newtab" => {
            let url = args.first().cloned();
            cmd_new_tab(ctx, url.as_deref()).await
        }
        "close" => cmd_close(ctx).await,
        "kill" => cmd_kill(ctx).await,
        "batch" => Box::pin(cmd_batch(ctx, args)).await,
        "media" => cmd_media(ctx, args).await,
        "animations" => cmd_animations(ctx, args.first().map(String::as_str)).await,
        "security" => cmd_security(ctx, args).await,
        "storage" => cmd_storage(ctx, args).await,
        "sw" => cmd_sw(ctx, args).await,
        "activate" => cmd_activate(ctx).await,
        "minimize" => cmd_minimize(ctx).await,
        "grid" => cmd_grid(ctx, args).await,
        // Legacy aliases for backwards compatibility with CLI usage
        "humanclick" => cmd_human_click_dispatch(ctx, args).await,
        "humantype" => {
            if args.len() < 2 {
                bail!("Usage: sidekar humantype <selector> <text>");
            }
            let selector = resolve_selector(ctx, &args[0])?;
            let text = args[1..].join(" ");
            cmd_human_type(ctx, &selector, &text).await
        }
        // Legacy aliases for doubleclick/rightclick CLI usage
        "doubleclick" => {
            if args.is_empty() {
                bail!("Usage: sidekar click --mode=double <sel|x,y|--text>");
            }
            cmd_double_click_dispatch(ctx, args).await
        }
        "rightclick" => {
            if args.is_empty() {
                bail!("Usage: sidekar click --mode=right <sel|x,y|--text>");
            }
            cmd_right_click_dispatch(ctx, args).await
        }
        "lock" => cmd_lock(ctx, args.first().map(String::as_str)).await,
        "unlock" => cmd_unlock(ctx).await,
        "search" => {
            let mut max_tokens = 0usize;
            let mut engine = None;
            let mut query_parts = Vec::new();
            for arg in args {
                if let Some(raw) = arg.strip_prefix("--tokens=") {
                    max_tokens = raw.parse::<usize>().unwrap_or(0);
                } else if let Some(raw) = arg.strip_prefix("--engine=") {
                    engine = Some(raw.to_string());
                } else {
                    query_parts.push(arg.clone());
                }
            }
            if query_parts.is_empty() {
                bail!("Usage: sidekar search <query> [--engine=google|bing|duckduckgo|<url>]");
            }
            cmd_search(ctx, &query_parts.join(" "), engine.as_deref(), max_tokens).await
        }
        "readurls" => {
            let mut max_tokens = 0usize;
            let mut urls = Vec::new();
            for arg in args {
                if let Some(raw) = arg.strip_prefix("--tokens=") {
                    max_tokens = raw.parse::<usize>().unwrap_or(0);
                } else {
                    urls.push(arg.clone());
                }
            }
            if urls.is_empty() {
                bail!("Usage: sidekar readurls <url1> <url2> ...");
            }
            cmd_readurls(ctx, &urls, max_tokens).await
        }
        "back" => cmd_back(ctx).await,
        "forward" => cmd_forward(ctx).await,
        "reload" => cmd_reload(ctx).await,
        "feedback" => {
            let rating: u8 = args.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            let comment = args.get(1).map(String::as_str).unwrap_or("");
            let config = crate::config::load_config();
            if !config.feedback {
                out!(
                    ctx,
                    "Feedback is disabled. Enable with: sidekar config set feedback true"
                );
                return Ok(());
            }
            if rating < 1 || rating > 5 {
                bail!("Rating must be 1-5");
            }
            match crate::api_client::send_feedback(
                &ctx.session_id,
                env!("CARGO_PKG_VERSION"),
                rating,
                comment,
            )
            .await
            {
                Ok(_) => out!(ctx, "Feedback sent. Thank you!"),
                Err(e) => out!(ctx, "Failed to send feedback: {e}"),
            }
            Ok(())
        }
        "errors" => {
            let n = args
                .first()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(50);
            let rows = crate::broker::error_events_recent(n)?;
            if rows.is_empty() {
                out!(ctx, "No rows in error_events (~/.sidekar/sidekar.sqlite3).");
                return Ok(());
            }
            out!(ctx, "id\tcreated_at\tsource\tmessage");
            for r in rows {
                let details = r.details.as_deref().unwrap_or("");
                let msg = if details.is_empty() {
                    r.message.clone()
                } else {
                    format!("{} | {}", r.message, details)
                };
                out!(
                    ctx,
                    "{}\t{}\t{}\t{}",
                    r.id,
                    r.created_at,
                    r.source,
                    msg
                );
            }
            Ok(())
        }
        "telemetry" => {
            let action = args.first().map(String::as_str).unwrap_or("status");
            let mut config = crate::config::load_config();
            match action {
                "on" | "enable" => {
                    config.telemetry = true;
                    crate::config::save_config(&config)?;
                    out!(ctx, "Telemetry enabled. Thank you!");
                }
                "off" | "disable" => {
                    config.telemetry = false;
                    crate::config::save_config(&config)?;
                    out!(ctx, "Telemetry disabled.");
                }
                "status" | "" => {
                    out!(ctx, "Telemetry: {}", if config.telemetry { "on" } else { "off" });
                }
                _ => bail!("Usage: sidekar telemetry [on|off|status]"),
            }
            Ok(())
        }
        "install" => cmd_setup(ctx).await,
        "uninstall" => cmd_uninstall(ctx).await,
        "config" => {
            let action = args.first().map(String::as_str).unwrap_or("list");
            match action {
                "list" | "ls" => {
                    let items = crate::config::config_list();
                    let max_key = items.iter().map(|(k, _, _)| k.len()).max().unwrap_or(0);
                    for (key, val, is_default) in &items {
                        let display_val = if val.is_empty() { "(not set)" } else { val.as_str() };
                        let marker = if *is_default { " (default)" } else { "" };
                        let desc = crate::config::find_key(key)
                            .map(|k| k.description)
                            .unwrap_or("");
                        out!(ctx, "{:<width$}  {}{}", key, display_val, marker, width = max_key);
                        if !desc.is_empty() {
                            out!(ctx, "{:<width$}  # {}", "", desc, width = max_key);
                        }
                    }
                    Ok(())
                }
                "get" => {
                    let key = args.get(1).map(String::as_str).unwrap_or("");
                    if key.is_empty() {
                        // No key specified — show all (same as list)
                        let items = crate::config::config_list();
                        let max_key = items.iter().map(|(k, _, _)| k.len()).max().unwrap_or(0);
                        for (key, val, is_default) in &items {
                            let display_val = if val.is_empty() { "(not set)" } else { val.as_str() };
                            let marker = if *is_default { " (default)" } else { "" };
                            out!(ctx, "{:<width$}  {}{}", key, display_val, marker, width = max_key);
                        }
                        return Ok(());
                    }
                    if crate::config::find_key(key).is_none() {
                        let valid: Vec<&str> = crate::config::CONFIG_KEYS.iter().map(|k| k.key).collect();
                        bail!("Unknown config key: {key}. Valid keys: {}", valid.join(", "));
                    }
                    let val = crate::config::config_get(key);
                    out!(ctx, "{}", if val.is_empty() { "(not set)".to_string() } else { val });
                    Ok(())
                }
                "set" => {
                    let key = args.get(1).map(String::as_str).unwrap_or("");
                    if key.is_empty() {
                        bail!("Usage: sidekar config set <key> <value>");
                    }
                    let ck = crate::config::find_key(key);
                    if ck.is_none() {
                        let valid: Vec<&str> = crate::config::CONFIG_KEYS.iter().map(|k| k.key).collect();
                        bail!("Unknown config key: {key}. Valid keys: {}", valid.join(", "));
                    }
                    let raw_value = args.get(2).map(String::as_str).unwrap_or("true");
                    // Browser validation
                    if key == "browser" {
                        if raw_value == "false" || raw_value == "none" || raw_value == "default" {
                            crate::config::config_delete(key)?;
                            out!(ctx, "Cleared browser preference (will use system default)");
                            return Ok(());
                        }
                        if find_browser_by_name(raw_value).is_none() {
                            bail!(
                                "Browser '{raw_value}' not found. Available: chrome, edge, brave, arc, vivaldi, chromium, canary"
                            );
                        }
                    }
                    crate::config::config_set(key, raw_value)?;
                    out!(ctx, "Set {key} = {raw_value}");
                    Ok(())
                }
                "reset" => {
                    let key = args.get(1).map(String::as_str).unwrap_or("");
                    if key.is_empty() {
                        bail!("Usage: sidekar config reset <key>");
                    }
                    if crate::config::find_key(key).is_none() {
                        let valid: Vec<&str> = crate::config::CONFIG_KEYS.iter().map(|k| k.key).collect();
                        bail!("Unknown config key: {key}. Valid keys: {}", valid.join(", "));
                    }
                    crate::config::config_delete(key)?;
                    let default = crate::config::find_key(key).unwrap().default;
                    let display = if default.is_empty() { "(not set)" } else { default };
                    out!(ctx, "Reset {key} to default: {display}");
                    Ok(())
                }
                _ => bail!("Usage: sidekar config [list|get <key>|set <key> <value>|reset <key>]"),
            }
        }
        "update" => {
            let current = env!("CARGO_PKG_VERSION");
            out!(ctx, "Current version: {current}");
            out!(ctx, "Checking for updates...");
            match crate::api_client::check_for_update().await {
                Ok(Some(latest)) => {
                    out!(ctx, "Update available: v{latest}");
                    out!(ctx, "Downloading...");
                    crate::api_client::self_update(&latest).await?;
                    match crate::daemon::restart_if_running() {
                        Ok(true) => out!(ctx, "Daemon restarted."),
                        Ok(false) => {}
                        Err(e) => out!(ctx, "Updated, but failed to restart daemon: {e}"),
                    }
                    out!(
                        ctx,
                        "Updated to v{latest}. Restart sidekar to use the new version."
                    );
                }
                Ok(None) => out!(ctx, "Already up to date (v{current})."),
                Err(e) => bail!("Failed to check for updates: {e}"),
            }
            Ok(())
        }
        "desktop-screenshot" | "desktop_screenshot" => cmd_desktop_screenshot(ctx, args).await,
        "desktop-apps" | "desktop_apps" => cmd_desktop_apps(ctx).await,
        "desktop-windows" | "desktop_windows" => cmd_desktop_windows(ctx, args).await,
        "desktop-find" | "desktop_find" => cmd_desktop_find(ctx, args).await,
        "desktop-click" | "desktop_click" => cmd_desktop_click(ctx, args).await,
        "desktop-launch" | "desktop_launch" => cmd_desktop_launch(ctx, args).await,
        "desktop-activate" | "desktop_activate" => cmd_desktop_activate(ctx, args).await,
        "desktop-quit" | "desktop_quit" => cmd_desktop_quit(ctx, args).await,
        "monitor" => cmd_monitor(ctx, args).await,
        // Bus tools — stateless CLI versions that recover identity from env/broker
        "who" => {
            let show_all = args.iter().any(|a| a == "--all" || a == "-a");
            let bus_state = recovered_bus_state();
            crate::bus::cmd_who(&bus_state, ctx, show_all)?;
            Ok(())
        }
        "bus_send" | "bus-send" => {
            if std::env::var("SIDEKAR_AGENT_NAME").is_err() {
                eprintln!("Warning: Not running inside sidekar wrapper. For full bus features, relaunch with: sidekar <agent-cli>");
            }
            let kind = args.iter()
                .find_map(|a| a.strip_prefix("--kind="))
                .unwrap_or("request");
            let filtered: Vec<&str> = args.iter()
                .filter(|a| !a.starts_with("--kind="))
                .map(String::as_str)
                .collect();
            let to = filtered.first().copied().unwrap_or_default();
            let message = if filtered.len() > 1 { filtered[1..].join(" ") } else { String::new() };
            if to.is_empty() || message.is_empty() {
                bail!("Usage: sidekar bus_send <to> <message> [--kind=request|fyi|response]");
            }
            let mut bus_state = recovered_bus_state();
            crate::bus::cmd_send_message(&mut bus_state, ctx, to, &message, kind, None)?;
            Ok(())
        }
        "bus_done" | "bus-done" => {
            if std::env::var("SIDEKAR_AGENT_NAME").is_err() {
                eprintln!("Warning: Not running inside sidekar wrapper. For full bus features, relaunch with: sidekar <agent-cli>");
            }
            if args.len() < 3 {
                bail!("Usage: sidekar bus_done <next> <summary> <request>");
            }
            let mut bus_state = recovered_bus_state();
            crate::bus::cmd_signal_done(&mut bus_state, ctx, &args[0], &args[1], &args[2], None)?;
            Ok(())
        }
        // Cron commands — CRUD operates on broker SQLite, execution runs in PTY wrapper
        "cron_create" | "cron-create" => {
            if args.len() < 2 {
                bail!("Usage: sidekar cron_create <schedule> <action_json> [--target=T] [--name=N]");
            }
            let schedule = &args[0];
            let action: serde_json::Value = serde_json::from_str(&args[1])
                .context("Invalid action JSON. Use: {\"tool\":\"screenshot\"} or {\"batch\":[...]}")?;
            let target = args.iter()
                .find_map(|a| a.strip_prefix("--target="))
                .unwrap_or("self");
            let name = args.iter()
                .find_map(|a| a.strip_prefix("--name="));
            let created_by = std::env::var("SIDEKAR_AGENT_NAME").unwrap_or_else(|_| "cli".into());
            let id = cron::cmd_cron_create(ctx, schedule, &action, target, name, &created_by).await?;
            let _ = id; // printed by cmd_cron_create
            Ok(())
        }
        "cron_list" | "cron-list" => {
            cron::cmd_cron_list(ctx).await
        }
        "cron_delete" | "cron-delete" => {
            let id = args.first().map(String::as_str).unwrap_or_default();
            if id.is_empty() {
                bail!("Usage: sidekar cron_delete <job-id>");
            }
            cron::cmd_cron_delete(ctx, id).await
        }
        // TOTP commands
        "totp" => {
            cmd_totp(ctx, args).await?;
            Ok(())
        }
        // KV store commands
        "kv" => {
            cmd_kv(ctx, args).await?;
            Ok(())
        }
        _ => bail!("Unknown command: {command}"),
    }
}

/// Build a bus state by recovering identity from SIDEKAR_AGENT_NAME env var + broker lookup.
/// Sets `borrowed = true` so the Drop impl won't unregister the PTY wrapper's agent.
fn recovered_bus_state() -> crate::bus::SidekarBusState {
    let mut state = crate::bus::SidekarBusState::new();
    if let Ok(name) = std::env::var("SIDEKAR_AGENT_NAME") {
        if let Ok(Some(agent)) = crate::broker::find_agent(&name, None) {
            state.identity = Some(agent.id);
            state.pane_unique_id = agent.pane_unique_id;
            state.inherited_pty = true;
            state.borrowed = true; // Don't unregister on drop — PTY wrapper owns this
            return state;
        }
        // Agent name is set but not found in broker — don't register a new one,
        // just return an unregistered state to avoid ghost agent spam.
        state.borrowed = true;
        return state;
    }
    // Fallback: try inheriting from parent PTY registration
    state.do_register(None);
    state
}
