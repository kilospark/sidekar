use crate::*;

mod agent_sessions;
mod agent_tools;
mod batch;
mod code;
mod core;
pub mod cron;
mod data;
mod desktop;
mod doc;
mod interaction;
pub mod kv;
pub mod monitor;
mod session;
mod system;
pub mod totp;

use agent_tools::*;
use batch::*;
use core::*;
use data::*;
use desktop::*;
use interaction::*;
use session::*;
use system::*;

// ---------------------------------------------------------------------------
// Argument Parsing Helpers
// ---------------------------------------------------------------------------

fn parse_selector_with_tokens(
    ctx: &AppContext,
    args: &[String],
    exclude_flags: &[&str],
) -> Result<(Option<String>, usize)> {
    let max_tokens = extract_optional_value(args, "--tokens=")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let selector_parts: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with("--tokens="))
        .filter(|a| !exclude_flags.iter().any(|f| *a == *f || a.starts_with(*f)))
        .map(String::as_str)
        .collect();

    let selector = if selector_parts.is_empty() {
        None
    } else {
        Some(resolve_selector(ctx, &selector_parts.join(" "))?)
    };

    Ok((selector, max_tokens))
}

fn extract_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn extract_optional_value(args: &[String], prefix: &str) -> Option<String> {
    args.iter()
        .find_map(|a| a.strip_prefix(prefix).map(|v| v.to_string()))
}

pub async fn dispatch(ctx: &mut AppContext, command: &str, args: &[String]) -> Result<()> {
    let command = crate::command_handler(command).unwrap_or(command);
    if let Some(result) = dispatch_system_command(ctx, command, args).await {
        return result;
    }
    if let Some(result) = dispatch_agent_command(ctx, command, args).await {
        return result;
    }
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
            let (selector, max_tokens) = parse_selector_with_tokens(ctx, args, &["--full"])?;
            cmd_dom(ctx, selector.as_deref(), max_tokens).await
        }
        "read" => {
            let (selector, max_tokens) = parse_selector_with_tokens(ctx, args, &[])?;
            cmd_read(ctx, selector.as_deref(), max_tokens).await
        }
        "text" => {
            let (selector, max_tokens) = parse_selector_with_tokens(ctx, args, &[])?;
            cmd_text(ctx, selector.as_deref(), max_tokens).await
        }
        "axtree" => {
            let interactive = extract_flag(args, "-i") || extract_flag(args, "--interactive");
            let diff = extract_flag(args, "--diff");
            let max_tokens = extract_optional_value(args, "--tokens=")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let selector = args
                .iter()
                .filter(|a| !matches!(a.as_str(), "-i" | "--interactive" | "--diff"))
                .find(|a| !a.starts_with("--tokens="))
                .map(|s| s.as_str());
            if interactive {
                cmd_axtree_interactive(ctx, max_tokens, diff).await
            } else {
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
                .context("Usage: sidekar wait-for <selector> [timeout_ms]")?;
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
        "tabs" => cmd_tabs(ctx, args).await,
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
        "geo" => cmd_geo(ctx, args).await,
        "mouse" => cmd_mouse(ctx, args).await,
        "state" => cmd_state(ctx, args).await,
        "auth" => cmd_auth(ctx, args).await,
        "screencast" => cmd_screencast(ctx, args).await,
        "activate" => cmd_activate(ctx).await,
        "minimize" => cmd_minimize(ctx).await,
        "grid" => cmd_grid(ctx, args).await,
        "lock" => cmd_lock(ctx, args.first().map(String::as_str)).await,
        "unlock" => cmd_unlock(ctx).await,
        "search" => {
            let engine = extract_optional_value(args, "--engine=").map(|v| v.to_string());
            let max_tokens = extract_optional_value(args, "--tokens=")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let query_parts: Vec<&str> = args
                .iter()
                .filter(|a| !a.starts_with("--engine=") && !a.starts_with("--tokens="))
                .map(String::as_str)
                .collect();
            if query_parts.is_empty() {
                bail!("Usage: sidekar search <query> [--engine=google|bing|duckduckgo|<url>]");
            }
            cmd_search(ctx, &query_parts.join(" "), engine.as_deref(), max_tokens).await
        }
        "readurls" => {
            let max_tokens = extract_optional_value(args, "--tokens=")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let urls: Vec<String> = args
                .iter()
                .filter(|a| !a.starts_with("--tokens="))
                .cloned()
                .collect();
            if urls.is_empty() {
                bail!("Usage: sidekar read-urls <url1> <url2> ...");
            }
            cmd_readurls(ctx, &urls, max_tokens).await
        }
        "back" => cmd_back(ctx).await,
        "forward" => cmd_forward(ctx).await,
        "reload" => cmd_reload(ctx).await,
        "desktop" => {
            let sub = args.first().map(String::as_str).unwrap_or("");
            let subcommand = match sub {
                "screenshot" => "desktop-screenshot",
                "apps" => "desktop-apps",
                "windows" => "desktop-windows",
                "find" => "desktop-find",
                "click" => "desktop-click",
                "press" => "desktop-press",
                "type" => "desktop-type",
                "paste" => "desktop-paste",
                "launch" => "desktop-launch",
                "activate" => "desktop-activate",
                "quit" => "desktop-quit",
                _ => bail!(
                    "Usage: sidekar desktop <screenshot|apps|windows|find|click|press|type|paste|launch|activate|quit> [args...]"
                ),
            };
            Box::pin(dispatch(ctx, subcommand, &args[1..])).await
        }
        "desktop-screenshot" => cmd_desktop_screenshot(ctx, args).await,
        "desktop-apps" => cmd_desktop_apps(ctx).await,
        "desktop-windows" => cmd_desktop_windows(ctx, args).await,
        "desktop-find" => cmd_desktop_find(ctx, args).await,
        "desktop-click" => cmd_desktop_click(ctx, args).await,
        "desktop-press" => cmd_desktop_press(ctx, args).await,
        "desktop-type" => cmd_desktop_type(ctx, args).await,
        "desktop-paste" => cmd_desktop_paste(ctx, args).await,
        "desktop-launch" => cmd_desktop_launch(ctx, args).await,
        "desktop-activate" => cmd_desktop_activate(ctx, args).await,
        "desktop-quit" => cmd_desktop_quit(ctx, args).await,
        _ => bail!("Unknown command: {command}"),
    }
}
