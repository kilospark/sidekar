use super::*;
#[cfg(target_os = "macos")]
use crate::output::{CommandOutput, PlainOutput};

pub(super) async fn cmd_desktop_screenshot(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    bail!("Desktop automation is only available on macOS");

    #[cfg(target_os = "macos")]
    {
        let mut pid: Option<i32> = None;
        let mut output_path: Option<PathBuf> = None;
        let mut format = "jpeg".to_string();
        let mut quality: u32 = 80;
        let mut target_width: Option<u32> = None;
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
                other => {
                    if let Some(v) = other.strip_prefix("--format=") {
                        format = if v == "png" {
                            "png".to_string()
                        } else {
                            "jpeg".to_string()
                        };
                    } else if let Some(v) = other.strip_prefix("--quality=") {
                        quality = v.parse().unwrap_or(80).clamp(1, 100);
                    } else if let Some(v) = other.strip_prefix("--width=") {
                        target_width = v.parse().ok();
                    }
                }
            }
            i += 1;
        }

        // Capture raw PNG to temp file
        let tmp_png = ctx.tmp_dir().join(format!(
            "sidekar-desktop-raw-{}.png",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));
        crate::desktop::screen::capture_desktop_screenshot(pid, &tmp_png).await?;

        // Load captured PNG and get dimensions
        let img = image::open(&tmp_png)
            .with_context(|| format!("failed to read screenshot {}", tmp_png.display()))?;
        let _ = std::fs::remove_file(&tmp_png);
        let pixel_w = img.width();
        let pixel_h = img.height();

        // Target width: explicit --width, or default 800px for token efficiency
        let target_w = target_width.unwrap_or(800).min(pixel_w);

        // Resize if needed
        let img = if target_w < pixel_w {
            img.resize(target_w, u32::MAX, image::imageops::FilterType::Lanczos3)
        } else {
            img
        };

        // Determine output path
        let ext = if format == "png" { "png" } else { "jpeg" };
        let out_path = output_path.unwrap_or_else(|| {
            ctx.tmp_dir().join(format!(
                "sidekar-desktop-{}.{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
                ext
            ))
        });

        // Write output in requested format
        if format == "png" {
            img.save(&out_path)
                .with_context(|| format!("failed to write {}", out_path.display()))?;
        } else {
            let mut out_file = std::fs::File::create(&out_path)
                .with_context(|| format!("failed to create {}", out_path.display()))?;
            let encoder =
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out_file, quality as u8);
            img.write_with_encoder(encoder)
                .context("failed to encode JPEG")?;
        }

        // Output metadata
        let file_kb = std::fs::metadata(&out_path)
            .map(|m| m.len() / 1024)
            .unwrap_or(0);
        let scaled_h = if target_w < pixel_w {
            (pixel_h as u64 * target_w as u64) / pixel_w as u64
        } else {
            pixel_h as u64
        };
        let est_tokens = (target_w as u64 * scaled_h) / 750;

        #[derive(serde::Serialize)]
        struct ScreenshotOutput {
            path: String,
            size_kb: u64,
            est_vision_tokens: u64,
        }
        impl CommandOutput for ScreenshotOutput {
            fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
                writeln!(w, "Screenshot saved to {}", self.path)?;
                writeln!(
                    w,
                    "Size: {}KB | Est. vision tokens: ~{}",
                    self.size_kb, self.est_vision_tokens
                )
            }
        }
        let output = ScreenshotOutput {
            path: out_path.display().to_string(),
            size_kb: file_kb,
            est_vision_tokens: est_tokens,
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
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
                "App '{}' not found. Run `sidekar desktop apps` to see running apps.",
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
        #[derive(serde::Serialize)]
        struct AppEntry {
            pid: i32,
            name: String,
            bundle_id: Option<String>,
            is_active: bool,
        }
        #[derive(serde::Serialize)]
        struct AppsOutput {
            apps: Vec<AppEntry>,
        }
        impl CommandOutput for AppsOutput {
            fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
                if self.apps.is_empty() {
                    writeln!(w, "No running applications found.")?;
                    return Ok(());
                }
                for app in &self.apps {
                    let active = if app.is_active { " *" } else { "" };
                    let bundle = app.bundle_id.as_deref().unwrap_or("-");
                    writeln!(w, "[{}] {} ({}){}", app.pid, app.name, bundle, active)?;
                }
                Ok(())
            }
        }
        let output = AppsOutput {
            apps: apps
                .into_iter()
                .map(|a| AppEntry {
                    pid: a.pid,
                    name: a.name,
                    bundle_id: a.bundle_id,
                    is_active: a.is_active,
                })
                .collect(),
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
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
        #[derive(serde::Serialize)]
        struct WindowEntry {
            title: Option<String>,
            x: f64,
            y: f64,
            width: f64,
            height: f64,
            window_id: Option<u32>,
            is_main: bool,
            is_focused: bool,
        }
        #[derive(serde::Serialize)]
        struct WindowsOutput {
            pid: i32,
            windows: Vec<WindowEntry>,
        }
        impl CommandOutput for WindowsOutput {
            fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
                if self.windows.is_empty() {
                    writeln!(w, "No windows found for pid {}.", self.pid)?;
                    return Ok(());
                }
                for win in &self.windows {
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
                    writeln!(
                        w,
                        "\"{}\" ({:.0}x{:.0} at {:.0},{:.0}){}{}",
                        title, win.width, win.height, win.x, win.y, wid, flags
                    )?;
                }
                Ok(())
            }
        }
        let output = WindowsOutput {
            pid,
            windows: windows
                .into_iter()
                .map(|win| WindowEntry {
                    title: win.title,
                    x: win.frame.x,
                    y: win.frame.y,
                    width: win.frame.width,
                    height: win.frame.height,
                    window_id: win.window_id,
                    is_main: win.is_main,
                    is_focused: win.is_focused,
                })
                .collect(),
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
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
            bail!("Usage: sidekar desktop find --app <name>|--pid <pid> <query>");
        }
        let matches = crate::desktop::native::find_elements(pid, &query)?;

        #[derive(serde::Serialize)]
        struct FindMatch {
            #[serde(skip_serializing_if = "Option::is_none")]
            ref_id: Option<String>,
            role: String,
            title: Option<String>,
            actions: Vec<String>,
        }
        #[derive(serde::Serialize)]
        struct FindOutput {
            query: String,
            matches: Vec<FindMatch>,
        }
        impl CommandOutput for FindOutput {
            fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
                if self.matches.is_empty() {
                    writeln!(w, "No elements found matching \"{}\"", self.query)?;
                    return Ok(());
                }
                writeln!(w, "Found {} element(s):", self.matches.len())?;
                for m in &self.matches {
                    let title = m.title.as_deref().unwrap_or("");
                    let actions = if m.actions.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", m.actions.join(", "))
                    };
                    let ref_tag = m
                        .ref_id
                        .as_deref()
                        .map(|r| format!("{} ", r))
                        .unwrap_or_default();
                    writeln!(w, "  {}{} \"{}\"{}", ref_tag, m.role, title, actions)?;
                }
                Ok(())
            }
        }
        let output = FindOutput {
            query: query.clone(),
            matches: matches
                .into_iter()
                .map(|m| FindMatch {
                    ref_id: m.ref_id,
                    role: m.role,
                    title: m.title,
                    actions: m.actions,
                })
                .collect(),
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
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
                // --query is passed by tool schema; consume it and add the value to rest
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

/// Like `parse_desktop_pid_and_rest` but doesn't require --app/--pid.
/// Returns `(None, all_args)` when neither is given.
#[cfg(target_os = "macos")]
fn parse_desktop_pid_and_rest_optional(args: &[String]) -> (Option<i32>, Vec<String>) {
    let mut pid: Option<i32> = None;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--app" => {
                i += 1;
                if let Some(name) = args.get(i) {
                    pid = resolve_pid_by_app_name(name).ok();
                }
            }
            "--pid" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    pid = v.parse().ok();
                }
            }
            "--query" => {
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
    (pid, rest)
}

pub(super) async fn cmd_desktop_launch(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    bail!("Desktop automation is only available on macOS");

    #[cfg(target_os = "macos")]
    {
        let name = args.join(" ");
        if name.is_empty() {
            bail!("Usage: sidekar desktop launch <app name>");
        }
        crate::desktop::native::launch_app(&name)?;
        out!(
            ctx,
            "{}",
            crate::output::to_string(&PlainOutput::new(format!("Launched {}", name)))?
        );
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
        out!(
            ctx,
            "{}",
            crate::output::to_string(&PlainOutput::new(format!("Activated app (pid {})", pid)))?
        );
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
        out!(
            ctx,
            "{}",
            crate::output::to_string(&PlainOutput::new(format!("Quit app (pid {})", pid)))?
        );
        Ok(())
    }
}

pub(super) async fn cmd_desktop_press(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    bail!("Desktop automation is only available on macOS");

    #[cfg(target_os = "macos")]
    {
        let (pid, remaining) = parse_desktop_pid_and_rest_optional(args);
        let spec = remaining.join(" ");
        if spec.is_empty() {
            bail!("Usage: sidekar desktop press [--app <name>|--pid <pid>] <key|combo>");
        }
        let keys: Vec<&str> = spec.split('+').map(|s| s.trim()).collect();
        crate::desktop::bg_input::hotkey(&keys, pid)?;
        let target = pid.map(|p| format!(" → pid {p}")).unwrap_or_default();
        out!(
            ctx,
            "{}",
            crate::output::to_string(&PlainOutput::new(format!("Pressed {}{}", spec, target)))?
        );
        Ok(())
    }
}

pub(super) async fn cmd_desktop_type(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    bail!("Desktop automation is only available on macOS");

    #[cfg(target_os = "macos")]
    {
        let (pid, remaining) = parse_desktop_pid_and_rest_optional(args);
        let text = remaining.join(" ");
        if text.is_empty() {
            bail!("Usage: sidekar desktop type [--app <name>|--pid <pid>] <text>");
        }
        crate::desktop::bg_input::type_characters(&text, 5, pid)?;
        let target = pid.map(|p| format!(" → pid {p}")).unwrap_or_default();
        out!(
            ctx,
            "{}",
            crate::output::to_string(&PlainOutput::new(format!(
                "Typed {} chars{}",
                text.chars().count(),
                target
            )))?
        );
        Ok(())
    }
}

pub(super) async fn cmd_desktop_scroll(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    bail!("Desktop automation is only available on macOS");

    #[cfg(target_os = "macos")]
    {
        let (pid, remaining) = parse_desktop_pid_and_rest_optional(args);
        let mut direction = String::from("down");
        let mut by = "line";
        let mut amount: u32 = 3;
        for arg in &remaining {
            match arg.as_str() {
                "up" | "down" | "left" | "right" => direction = arg.clone(),
                "page" => by = "page",
                "line" => by = "line",
                _ => {
                    if let Ok(n) = arg.parse::<u32>() {
                        amount = n;
                    } else if let Some(v) = arg.strip_prefix("--amount=") {
                        amount = v.parse().unwrap_or(3);
                    } else if let Some(v) = arg.strip_prefix("--by=") {
                        by = if v == "page" { "page" } else { "line" };
                    }
                }
            }
        }
        let dir = match direction.as_str() {
            "up" => crate::desktop::bg_input::ScrollDirection::Up,
            "down" => crate::desktop::bg_input::ScrollDirection::Down,
            "left" => crate::desktop::bg_input::ScrollDirection::Left,
            "right" => crate::desktop::bg_input::ScrollDirection::Right,
            _ => crate::desktop::bg_input::ScrollDirection::Down,
        };
        let gran = if by == "page" {
            crate::desktop::bg_input::ScrollGranularity::Page
        } else {
            crate::desktop::bg_input::ScrollGranularity::Line
        };
        crate::desktop::bg_input::scroll(dir, gran, amount, pid)?;
        let target = pid.map(|p| format!(" → pid {p}")).unwrap_or_default();
        out!(
            ctx,
            "{}",
            crate::output::to_string(&PlainOutput::new(format!(
                "Scrolled {direction} {amount}× ({by}){target}"
            )))?
        );
        Ok(())
    }
}

pub(super) async fn cmd_desktop_paste(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    bail!("Desktop automation is only available on macOS");

    #[cfg(target_os = "macos")]
    {
        let (pid, remaining) = parse_desktop_pid_and_rest_optional(args);
        let text = remaining.join(" ");
        if text.is_empty() {
            bail!("Usage: sidekar desktop paste [--app <name>|--pid <pid>] <text>");
        }
        crate::desktop::input::set_clipboard_text(&text)?;
        crate::desktop::bg_input::hotkey(&["cmd", "v"], pid)?;
        let target = pid.map(|p| format!(" → pid {p}")).unwrap_or_default();
        out!(
            ctx,
            "{}",
            crate::output::to_string(&PlainOutput::new(format!(
                "Pasted {} chars{}",
                text.chars().count(),
                target
            )))?
        );
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
            bail!("Usage: sidekar desktop click --app <name>|--pid <pid> <query>");
        }

        let result = crate::desktop::native::click_element(pid, &query)?;
        let msg = match result.kind.as_str() {
            "axPress" => {
                let role = result.role.as_deref().unwrap_or("element");
                let title = result.title.as_deref().unwrap_or("");
                format!("Clicked {} \"{}\"", role, title)
            }
            "fallbackClick" => {
                if let (Some(x), Some(y)) = (result.x, result.y) {
                    // Use bg_input for per-pid click when we have a target pid
                    crate::desktop::bg_input::click_at_pid(
                        x,
                        y,
                        pid,
                        crate::desktop::bg_input::MouseButton::Left,
                        1,
                        None,
                    )?;
                    let role = result.role.as_deref().unwrap_or("element");
                    let title = result.title.as_deref().unwrap_or("");
                    format!(
                        "Clicked {} \"{}\" at ({:.0}, {:.0}) via coordinate fallback",
                        role, title, x, y
                    )
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
        };
        out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
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
                    crate::desktop::bg_input::click_at_pid(
                        x,
                        y,
                        pid,
                        crate::desktop::bg_input::MouseButton::Left,
                        1,
                        None,
                    )?;
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

pub(super) async fn cmd_desktop_trust(ctx: &mut AppContext, _args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        bail!("Desktop trust check is only available on macOS");
    }

    #[cfg(target_os = "macos")]
    {
        let status = crate::desktop::native::trust_status();
        let is_json = matches!(
            crate::runtime::output_format(),
            crate::output::OutputFormat::Json
        );
        if is_json {
            let obj = serde_json::json!({
                "accessibility": status.accessibility.as_str(),
                "screenRecording": status.screen_recording.as_str(),
                "microphone": status.microphone.as_str(),
            });
            out!(ctx, "{}", serde_json::to_string_pretty(&obj)?);
        } else {
            let path = |label: &str| format!("    System Settings → Privacy & Security → {label}");
            let lines = format!(
                "macOS permissions:\n\
                 \n\
                 Accessibility    : {ax}\n{ax_path}\n\
                 Screen Recording : {sr}\n{sr_path}\n\
                 Microphone       : {mic}\n{mic_path}\n\
                 \n\
                 Accessibility is required for AX-based element targeting and\n\
                 reliable OS-level input. Screen Recording is required for window\n\
                 titles and screen capture beyond the current app. Microphone is\n\
                 only needed if sidekar drives speech or audio-capable features.",
                ax = status.accessibility.as_str(),
                ax_path = path("Accessibility"),
                sr = status.screen_recording.as_str(),
                sr_path = path("Screen Recording"),
                mic = status.microphone.as_str(),
                mic_path = path("Microphone"),
            );
            out!(
                ctx,
                "{}",
                crate::output::to_string(&PlainOutput::new(lines))?
            );
        }
        Ok(())
    }
}

pub(super) async fn cmd_desktop_clipboard(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        bail!("Desktop clipboard is only available on macOS");
    }

    #[cfg(target_os = "macos")]
    {
        let sub = args.first().map(String::as_str).unwrap_or("read");
        match sub {
            "read" | "get" => {
                let text = crate::desktop::input::read_clipboard_text()?;
                out!(ctx, "{text}");
            }
            "write" | "set" => {
                let text = args
                    .get(1)
                    .cloned()
                    .ok_or_else(|| anyhow!("Usage: sidekar desktop clipboard write <text>"))?;
                crate::desktop::input::set_clipboard_text(&text)?;
                out!(
                    ctx,
                    "{}",
                    crate::output::to_string(&PlainOutput::new(format!(
                        "Wrote {} chars to clipboard.",
                        text.len()
                    )))?
                );
            }
            other => bail!("Unknown clipboard subcommand: {other} (use read|write)"),
        }
        Ok(())
    }
}

pub(super) async fn cmd_desktop_menu(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        bail!("Desktop menu is only available on macOS");
    }

    #[cfg(target_os = "macos")]
    {
        let mut pid: Option<i32> = None;
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
                _ => {}
            }
            i += 1;
        }
        let pid = pid
            .or_else(crate::desktop::native::frontmost_app_pid)
            .ok_or_else(|| anyhow!("No app specified; pass --app or --pid"))?;
        let entries = crate::desktop::native::list_menu(pid)?;
        if entries.is_empty() {
            out!(
                ctx,
                "{}",
                crate::output::to_string(&PlainOutput::new(
                    "No menu entries (app may not have a menu bar or permission denied).",
                ))?
            );
        } else {
            out!(ctx, "{}", entries.join("\n"));
        }
        Ok(())
    }
}

pub(super) async fn cmd_desktop_check_bg(ctx: &mut AppContext, _args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    bail!("Desktop automation is only available on macOS");

    #[cfg(target_os = "macos")]
    {
        let skylight = crate::desktop::skylight::is_available();
        let focus_wr = crate::desktop::skylight::is_focus_without_raise_available();
        let win_loc = crate::desktop::skylight::is_window_location_available();

        #[derive(serde::Serialize)]
        struct BgStatus {
            skylight_event_post: bool,
            focus_without_raise: bool,
            window_location: bool,
            background_input_ready: bool,
        }
        impl CommandOutput for BgStatus {
            fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
                writeln!(w, "Background desktop automation:")?;
                writeln!(
                    w,
                    "  SLEventPostToPid + auth message : {}",
                    if self.skylight_event_post {
                        "✓"
                    } else {
                        "✗"
                    }
                )?;
                writeln!(
                    w,
                    "  FocusWithoutRaise (SLPSPost)    : {}",
                    if self.focus_without_raise {
                        "✓"
                    } else {
                        "✗"
                    }
                )?;
                writeln!(
                    w,
                    "  CGEventSetWindowLocation        : {}",
                    if self.window_location { "✓" } else { "✗" }
                )?;
                writeln!(
                    w,
                    "  Background input ready          : {}",
                    if self.background_input_ready {
                        "✓"
                    } else {
                        "✗"
                    }
                )?;
                if !self.background_input_ready {
                    writeln!(
                        w,
                        "\n  Background input requires macOS 14+ with SkyLight SPI.\n  \
                         Falling back to foreground input (cursor will move)."
                    )?;
                }
                Ok(())
            }
        }
        let output = BgStatus {
            skylight_event_post: skylight,
            focus_without_raise: focus_wr,
            window_location: win_loc,
            background_input_ready: skylight && focus_wr,
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
        Ok(())
    }
}

pub(super) async fn cmd_desktop_monitor(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        bail!("Desktop monitor is only available on macOS");
    }

    #[cfg(target_os = "macos")]
    {
        let sub = args.first().map(String::as_str).unwrap_or("stats");
        let mut limit: Option<u64> = None;
        let mut i = 1;
        while i < args.len() {
            if (args[i] == "-n" || args[i] == "--limit")
                && let Some(v) = args.get(i + 1).and_then(|s| s.parse::<u64>().ok())
            {
                limit = Some(v);
                i += 2;
                continue;
            }
            i += 1;
        }
        let mut cmd = serde_json::json!({
            "type": "desktop_monitor",
            "action": sub,
        });
        if let Some(l) = limit {
            cmd["limit"] = serde_json::json!(l);
        }
        let resp = crate::daemon::send_command(&cmd)
            .context("daemon unreachable — try `sidekar daemon start`")?;
        if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
            bail!("{}", err);
        }

        let is_json = matches!(
            crate::runtime::output_format(),
            crate::output::OutputFormat::Json
        );
        match sub {
            "start" => {
                out!(
                    ctx,
                    "{}",
                    crate::output::to_string(&PlainOutput::new(
                        "Monitor started. Use `sidekar desktop monitor tail` to read.",
                    ))?
                );
            }
            "stop" => {
                out!(
                    ctx,
                    "{}",
                    crate::output::to_string(&PlainOutput::new("Monitor stopped."))?
                );
            }
            "clear" => {
                let n = resp.get("cleared").and_then(|v| v.as_u64()).unwrap_or(0);
                out!(ctx, "Cleared {n} events.");
            }
            "stats" => {
                out!(ctx, "{}", serde_json::to_string_pretty(&resp)?);
            }
            "log" | "tail" => {
                let events = resp
                    .get("events")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                if is_json {
                    out!(ctx, "{}", serde_json::to_string_pretty(&events)?);
                } else if events.is_empty() {
                    out!(ctx, "No events captured.");
                } else {
                    for e in events {
                        let k = e.get("k").and_then(|v| v.as_str()).unwrap_or("?");
                        let t = e.get("t").and_then(|v| v.as_u64()).unwrap_or(0);
                        let x = e.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let y = e.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let kc = e.get("keycode").and_then(|v| v.as_i64()).unwrap_or(-1);
                        let kc_str = if kc >= 0 {
                            format!(" key={kc}")
                        } else {
                            String::new()
                        };
                        out!(ctx, "[{t}] {k} ({x:.0}, {y:.0}){kc_str}");
                    }
                }
            }
            other => bail!("Unknown monitor subcommand: {other}"),
        }
        Ok(())
    }
}

pub(super) async fn cmd_desktop_monitor_watch(
    _ctx: &mut AppContext,
    _args: &[String],
) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        bail!("Desktop monitor is only available on macOS");
    }

    #[cfg(target_os = "macos")]
    {
        // Runs the CGEventTap in THIS process (the CLI). The CLI has the
        // user-interactive TCC context that the detached daemon lacks, so
        // Accessibility grants actually apply here. Streams events to stdout
        // until Ctrl-C. No buffer — each event prints as it arrives.
        use std::time::Duration;
        use tokio::time::sleep;

        crate::desktop::monitor::start()?;
        eprintln!("Monitor running in foreground. Ctrl-C to stop.");

        loop {
            let events = crate::desktop::monitor::snapshot(None);
            crate::desktop::monitor::clear();
            for e in &events {
                let kc = if e.keycode >= 0 {
                    format!(" key={}", e.keycode)
                } else {
                    String::new()
                };
                println!("[{}] {} ({:.0}, {:.0}){}", e.t_ms, e.kind, e.x, e.y, kc);
            }
            sleep(Duration::from_millis(100)).await;
        }
    }
}
