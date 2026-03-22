use super::*;

pub(super) async fn cmd_desktop_screenshot(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    bail!("Desktop automation is only available on macOS");

    #[cfg(target_os = "macos")]
    {
        let mut pid: Option<i32> = None;
        let mut output_path: Option<PathBuf> = None;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--app" => {
                    i += 1;
                    let name = args.get(i).context("--app requires a name")?;
                    pid = Some(resolve_pid_by_app_name(name)?);
                }
                "--pid" => {
                    i += 1;
                    pid = Some(
                        args.get(i)
                            .context("--pid requires a value")?
                            .parse()
                            .context("invalid pid")?,
                    );
                }
                "--output" => {
                    i += 1;
                    output_path = Some(PathBuf::from(
                        args.get(i).context("--output requires a path")?,
                    ));
                }
                _ => {}
            }
            i += 1;
        }

        let path = output_path.unwrap_or_else(|| {
            ctx.tmp_dir().join(format!(
                "sidekar-desktop-{}.png",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
            ))
        });

        crate::desktop::screen::capture_desktop_screenshot(pid, &path).await?;
        out!(ctx, "Screenshot saved to {}", path.display());
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn resolve_pid_by_app_name(name: &str) -> Result<i32> {
    let apps = crate::desktop::native::list_apps()?;
    let lower = name.to_lowercase();
    apps.iter()
        .find(|a| a.name.to_lowercase().contains(&lower))
        .map(|a| a.pid)
        .ok_or_else(|| {
            anyhow!(
                "App '{}' not found. Run desktop-apps to see running apps.",
                name
            )
        })
}

#[cfg(not(target_os = "macos"))]
fn resolve_pid_by_app_name(_name: &str) -> Result<i32> {
    bail!("Desktop automation is only available on macOS")
}

#[cfg(target_os = "macos")]
fn parse_desktop_pid(args: &[String]) -> Result<i32> {
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--app" => {
                i += 1;
                let name = args.get(i).context("--app requires a name")?;
                return resolve_pid_by_app_name(name);
            }
            "--pid" => {
                i += 1;
                return args
                    .get(i)
                    .context("--pid requires a value")?
                    .parse()
                    .context("invalid pid");
            }
            _ => {}
        }
        i += 1;
    }
    bail!("Required: --app <name> or --pid <pid>")
}

pub(super) async fn cmd_desktop_apps(ctx: &mut AppContext) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    bail!("Desktop automation is only available on macOS");

    #[cfg(target_os = "macos")]
    {
        let apps = crate::desktop::native::list_apps()?;
        if apps.is_empty() {
            out!(ctx, "No running applications found.");
        } else {
            for app in &apps {
                let active = if app.is_active { " *" } else { "" };
                let bundle = app.bundle_id.as_deref().unwrap_or("-");
                out!(ctx, "[{}] {} ({}){}", app.pid, app.name, bundle, active);
            }
        }
        Ok(())
    }
}

pub(super) async fn cmd_desktop_windows(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    bail!("Desktop automation is only available on macOS");

    #[cfg(target_os = "macos")]
    {
        let pid = parse_desktop_pid(args)?;
        let windows = crate::desktop::native::list_windows(pid)?;
        if windows.is_empty() {
            out!(ctx, "No windows found for pid {pid}.");
        } else {
            for win in &windows {
                let title = win.title.as_deref().unwrap_or("(untitled)");
                let flags = match (win.is_main, win.is_focused) {
                    (true, true) => " [main, focused]",
                    (true, false) => " [main]",
                    (false, true) => " [focused]",
                    _ => "",
                };
                let wid = win
                    .window_id
                    .map(|id| format!(" wid:{id}"))
                    .unwrap_or_default();
                out!(
                    ctx,
                    "\"{}\" ({:.0}x{:.0} at {:.0},{:.0}){}{}",
                    title,
                    win.frame.width,
                    win.frame.height,
                    win.frame.x,
                    win.frame.y,
                    wid,
                    flags
                );
            }
        }
        Ok(())
    }
}

pub(super) async fn cmd_desktop_find(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    bail!("Desktop automation is only available on macOS");

    #[cfg(target_os = "macos")]
    {
        let (pid, remaining) = parse_desktop_pid_and_rest(args)?;
        let query = remaining.join(" ");
        if query.is_empty() {
            bail!("Usage: desktop-find --app <name>|--pid <pid> <query>");
        }
        let matches = crate::desktop::native::find_elements(pid, &query)?;
        if matches.is_empty() {
            out!(ctx, "No elements found matching \"{}\"", query);
        } else {
            out!(ctx, "Found {} element(s):", matches.len());
            for m in &matches {
                let title = m.title.as_deref().unwrap_or("");
                let actions = if m.actions.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", m.actions.join(", "))
                };
                out!(ctx, "  {} \"{}\"{}", m.role, title, actions);
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn parse_desktop_pid_and_rest(args: &[String]) -> Result<(i32, Vec<String>)> {
    let mut pid: Option<i32> = None;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--app" => {
                i += 1;
                let name = args.get(i).context("--app requires a name")?;
                pid = Some(resolve_pid_by_app_name(name)?);
            }
            "--pid" => {
                i += 1;
                pid = Some(
                    args.get(i)
                        .context("--pid requires a value")?
                        .parse()
                        .context("invalid pid")?,
                );
            }
            "--query" => {
                // --query is passed by MCP tool schema; consume it and add the value to rest
                i += 1;
                if let Some(v) = args.get(i) {
                    rest.push(v.to_string());
                }
            }
            other => {
                rest.push(other.to_string());
            }
        }
        i += 1;
    }
    let pid = pid.ok_or_else(|| anyhow!("Required: --app <name> or --pid <pid>"))?;
    Ok((pid, rest))
}

pub(super) async fn cmd_desktop_launch(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    bail!("Desktop automation is only available on macOS");

    #[cfg(target_os = "macos")]
    {
        let name = args.join(" ");
        if name.is_empty() {
            bail!("Usage: desktop-launch <app name>");
        }
        crate::desktop::native::launch_app(&name)?;
        out!(ctx, "Launched {}", name);
        Ok(())
    }
}

pub(super) async fn cmd_desktop_activate(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    bail!("Desktop automation is only available on macOS");

    #[cfg(target_os = "macos")]
    {
        let pid = parse_desktop_pid(args)?;
        crate::desktop::native::activate_app(pid)?;
        out!(ctx, "Activated app (pid {})", pid);
        Ok(())
    }
}

pub(super) async fn cmd_desktop_quit(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    bail!("Desktop automation is only available on macOS");

    #[cfg(target_os = "macos")]
    {
        let pid = parse_desktop_pid(args)?;
        crate::desktop::native::quit_app(pid)?;
        out!(ctx, "Quit app (pid {})", pid);
        Ok(())
    }
}

pub(super) async fn cmd_desktop_click(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    bail!("Desktop automation is only available on macOS");

    #[cfg(target_os = "macos")]
    {
        let (pid, remaining) = parse_desktop_pid_and_rest(args)?;
        let query = remaining.join(" ");
        if query.is_empty() {
            bail!("Usage: desktop-click --app <name>|--pid <pid> <query>");
        }

        let result = crate::desktop::native::click_element(pid, &query)?;
        match result.kind.as_str() {
            "axPress" => {
                let role = result.role.as_deref().unwrap_or("element");
                let title = result.title.as_deref().unwrap_or("");
                out!(ctx, "Clicked {} \"{}\"", role, title);
            }
            "fallbackClick" => {
                if let (Some(x), Some(y)) = (result.x, result.y) {
                    crate::desktop::input::click_at(x, y)?;
                    let role = result.role.as_deref().unwrap_or("element");
                    let title = result.title.as_deref().unwrap_or("");
                    out!(
                        ctx,
                        "Clicked {} \"{}\" at ({:.0}, {:.0}) via coordinate fallback",
                        role,
                        title,
                        x,
                        y
                    );
                } else {
                    bail!("Element found but no coordinates available for fallback click");
                }
            }
            "notFound" => {
                bail!("No element found matching \"{}\"", query);
            }
            "noFrame" => {
                bail!("Element found but has no position — cannot click");
            }
            other => {
                bail!("Unexpected click result: {}", other);
            }
        }
        Ok(())
    }
}

/// Try to click an element in the browser via the desktop accessibility API.
/// Returns Ok with a description if the click succeeded, Err if it failed.
/// Only available on macOS; returns Err immediately on other platforms.
pub(crate) fn try_desktop_click_fallback(browser_name: &str, query: &str) -> Result<String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (browser_name, query);
        bail!("not available");
    }

    #[cfg(target_os = "macos")]
    {
        let pid = resolve_pid_by_app_name(browser_name)?;
        let result = crate::desktop::native::click_element(pid, query)?;
        match result.kind.as_str() {
            "axPress" => {
                let role = result.role.as_deref().unwrap_or("element");
                let title = result.title.as_deref().unwrap_or("");
                Ok(format!(
                    "Clicked {} \"{}\" via desktop fallback",
                    role, title
                ))
            }
            "fallbackClick" => {
                if let (Some(x), Some(y)) = (result.x, result.y) {
                    crate::desktop::input::click_at(x, y)?;
                    let role = result.role.as_deref().unwrap_or("element");
                    let title = result.title.as_deref().unwrap_or("");
                    Ok(format!(
                        "Clicked {} \"{}\" at ({:.0}, {:.0}) via desktop fallback",
                        role, title, x, y
                    ))
                } else {
                    bail!("no coordinates for fallback");
                }
            }
            _ => bail!("desktop click failed: {}", result.kind),
        }
    }
}
