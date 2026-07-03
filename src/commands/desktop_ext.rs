//! Extended desktop automation commands (Peekaboo-inspired features).

use super::*;
#[cfg(target_os = "macos")]
use crate::output::PlainOutput;

#[cfg(target_os = "macos")]
pub(super) async fn cmd_desktop_see(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let (pid, _) = parse_desktop_pid_and_rest(args)?;
    let mut annotate = false;
    let mut width: Option<u32> = None;
    for arg in args {
        match arg.as_str() {
            "--annotate" => annotate = true,
            other => {
                if let Some(v) = other.strip_prefix("--width=") {
                    width = v.parse().ok();
                }
            }
        }
    }
    let snapshot =
        crate::desktop::build_see_snapshot(pid, &ctx.tmp_dir(), annotate, width, 12, 200).await?;
    crate::desktop::persist_snapshot(&snapshot)?;
    out!(ctx, "{}", crate::output::to_string(&snapshot)?);
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) async fn cmd_desktop_set_value(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let (pid, remaining) = parse_desktop_pid_and_rest(args)?;
    let mut target: Option<String> = None;
    let mut value = String::new();
    let mut i = 0;
    while i < remaining.len() {
        match remaining[i].as_str() {
            "--on" => {
                i += 1;
                target = remaining.get(i).cloned();
                i += 1;
            }
            other => {
                if !value.is_empty() {
                    value.push(' ');
                }
                value.push_str(other);
                i += 1;
            }
        }
    }
    let target = target.context("--on <@eN|query> required")?;
    if value.is_empty() {
        bail!("Usage: sidekar desktop set-value --app <name>|--pid <pid> --on <target> <value>");
    }
    let resolved = crate::desktop::resolve_target(pid, &target)?;
    let result = crate::desktop::set_value_on_target(&resolved, &value)?;
    out!(ctx, "{}", crate::output::to_string(&result)?);
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) async fn cmd_desktop_perform_action(
    ctx: &mut AppContext,
    args: &[String],
) -> Result<()> {
    let (pid, remaining) = parse_desktop_pid_and_rest(args)?;
    let mut target = None;
    let mut action = None;
    let mut i = 0;
    while i < remaining.len() {
        match remaining[i].as_str() {
            "--on" => {
                i += 1;
                target = remaining.get(i).cloned();
                i += 1;
            }
            "--action" => {
                i += 1;
                action = remaining.get(i).cloned();
                i += 1;
            }
            other if !other.starts_with("--") && target.is_none() => {
                target = Some(other.to_string());
                i += 1;
            }
            other if !other.starts_with("--") && action.is_none() => {
                action = Some(other.to_string());
                i += 1;
            }
            _ => i += 1,
        }
    }
    let target = target.context("--on <@eN|query> required")?;
    let action = action.context("--action <AXAction> required")?;
    let resolved = crate::desktop::resolve_target(pid, &target)?;
    let result = crate::desktop::perform_action_on_target(&resolved, &action)?;
    out!(ctx, "{}", crate::output::to_string(&result)?);
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) async fn cmd_desktop_menu_ext(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.first().map(String::as_str) == Some("click") {
        return cmd_desktop_menu_click(ctx, &args[1..]).await;
    }
    cmd_desktop_menu_list(ctx, args).await
}

#[cfg(target_os = "macos")]
async fn cmd_desktop_menu_list(ctx: &mut AppContext, args: &[String]) -> Result<()> {
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
                pid = Some(args.get(i).context("--pid requires a value")?.parse()?);
            }
            "list" => {}
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

#[cfg(target_os = "macos")]
async fn cmd_desktop_menu_click(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let mut pid: Option<i32> = None;
    let mut path: Option<String> = None;
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
                pid = Some(args.get(i).context("--pid requires a value")?.parse()?);
            }
            "--path" => {
                i += 1;
                path = args.get(i).cloned();
            }
            other if path.is_none() && !other.starts_with("--") => {
                path = Some(other.to_string());
            }
            _ => {}
        }
        i += 1;
    }
    let pid = pid
        .or_else(crate::desktop::native::frontmost_app_pid)
        .ok_or_else(|| anyhow!("No app specified; pass --app or --pid"))?;
    let path =
        path.context("Usage: sidekar desktop menu click --app <name> --path \"File > New\"")?;
    let msg = crate::desktop::click_menu_path(pid, &path)?;
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) async fn cmd_desktop_dialog(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    let mut pid: Option<i32> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--app" => {
                i += 1;
                let name = rest.get(i).context("--app requires a name")?;
                pid = Some(resolve_pid_by_app_name(name)?);
            }
            "--pid" => {
                i += 1;
                pid = Some(rest.get(i).context("--pid requires a value")?.parse()?);
            }
            _ => break,
        }
        i += 1;
    }
    let pid = pid
        .or_else(crate::desktop::native::frontmost_app_pid)
        .ok_or_else(|| anyhow!("No app specified; pass --app or --pid"))?;
    match sub {
        "list" => {
            let info = crate::desktop::native::find_dialog_info(pid)?;
            out!(ctx, "{}", crate::output::to_string(&info)?);
            Ok(())
        }
        "click" => {
            let label = rest
                .windows(2)
                .find(|w| w[0] == "--button")
                .map(|w| w[1].as_str())
                .or_else(|| {
                    rest.iter()
                        .find(|a| !a.starts_with("--") && *a != "click")
                        .map(|s| s.as_str())
                })
                .context("Usage: sidekar desktop dialog click --button <label>")?;
            let msg = crate::desktop::native::click_dialog_button(pid, label)?;
            out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
            Ok(())
        }
        "input" => {
            let text = rest
                .iter()
                .find(|a| !a.starts_with("--") && *a != "input")
                .context("Usage: sidekar desktop dialog input <text> [--field <label>]")?;
            let field = rest
                .windows(2)
                .find(|w| w[0] == "--field")
                .map(|w| w[1].as_str());
            let msg = crate::desktop::native::set_dialog_field(pid, text, field, None)?;
            out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
            Ok(())
        }
        "dismiss" => {
            let force = rest.iter().any(|a| a == "--force");
            let msg = crate::desktop::native::dismiss_dialog(pid, force)?;
            out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
            Ok(())
        }
        other => bail!("Unknown dialog subcommand: {other}"),
    }
}

#[cfg(target_os = "macos")]
pub(super) async fn cmd_desktop_window(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    let mut pid: Option<i32> = None;
    let mut window_index = 0usize;
    let mut x = None;
    let mut y = None;
    let mut width = None;
    let mut height = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--app" => {
                i += 1;
                pid = Some(resolve_pid_by_app_name(rest.get(i).context("--app")?)?);
            }
            "--pid" => {
                i += 1;
                pid = Some(rest.get(i).context("--pid")?.parse()?);
            }
            "--index" => {
                i += 1;
                window_index = rest.get(i).context("--index")?.parse()?;
            }
            "--x" => {
                i += 1;
                x = Some(rest.get(i).context("--x")?.parse()?);
            }
            "--y" => {
                i += 1;
                y = Some(rest.get(i).context("--y")?.parse()?);
            }
            "--width" => {
                i += 1;
                width = Some(rest.get(i).context("--width")?.parse()?);
            }
            "--height" => {
                i += 1;
                height = Some(rest.get(i).context("--height")?.parse()?);
            }
            _ => {}
        }
        i += 1;
    }
    let pid = pid.context("--app or --pid required")?;
    match sub {
        "list" => {
            let wins = crate::desktop::native::list_windows(pid)?;
            out!(
                ctx,
                "{}",
                crate::output::to_string(&crate::desktop::DesktopWindowListOutput {
                    windows: wins
                })?
            );
            Ok(())
        }
        "focus" => {
            let window = crate::desktop::native::window_element_at(pid, window_index)?;
            crate::desktop::native::raise_window(window)?;
            crate::desktop::native::release_ax_element(window);
            out!(
                ctx,
                "{}",
                crate::output::to_string(&PlainOutput::new(format!(
                    "Focused window {window_index} on pid {pid}"
                )))?
            );
            Ok(())
        }
        "close" => {
            let window = crate::desktop::native::window_element_at(pid, window_index)?;
            crate::desktop::native::close_window(window)?;
            crate::desktop::native::release_ax_element(window);
            out!(
                ctx,
                "{}",
                crate::output::to_string(&PlainOutput::new(format!(
                    "Closed window {window_index} on pid {pid}"
                )))?
            );
            Ok(())
        }
        "move" | "resize" | "set-bounds" => {
            let msg =
                crate::desktop::native::set_window_bounds(pid, window_index, x, y, width, height)?;
            out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
            Ok(())
        }
        other => bail!("Unknown window subcommand: {other}"),
    }
}

#[cfg(target_os = "macos")]
pub(super) async fn cmd_desktop_space(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    match sub {
        "list" => {
            let spaces = crate::desktop::spaces::list_spaces()?;
            out!(
                ctx,
                "{}",
                crate::output::to_string(&crate::desktop::DesktopSpaceListOutput { spaces })?
            );
            Ok(())
        }
        "switch" => {
            let index: usize = rest
                .iter()
                .find_map(|a| {
                    a.strip_prefix("--to=").or_else(|| {
                        if !a.starts_with("--") {
                            Some(a.as_str())
                        } else {
                            None
                        }
                    })
                })
                .context("Usage: sidekar desktop space switch --to <1-9>")?
                .parse()?;
            let msg = crate::desktop::spaces::switch_space(index)?;
            out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
            Ok(())
        }
        "move-window" => {
            let mut pid = None;
            let mut window_index = 0usize;
            let mut to = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--app" => {
                        i += 1;
                        pid = Some(resolve_pid_by_app_name(rest.get(i).context("--app")?)?);
                    }
                    "--pid" => {
                        i += 1;
                        pid = Some(rest.get(i).context("--pid")?.parse()?);
                    }
                    "--index" => {
                        i += 1;
                        window_index = rest.get(i).context("--index")?.parse()?;
                    }
                    "--to" => {
                        i += 1;
                        to = Some(rest.get(i).context("--to")?.parse()?);
                    }
                    _ => {}
                }
                i += 1;
            }
            let pid = pid.context("--app or --pid required")?;
            let to = to.context("--to <1-9> required")?;
            let msg = crate::desktop::spaces::move_window_to_space(pid, window_index, to)?;
            out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
            Ok(())
        }
        other => bail!("Unknown space subcommand: {other}"),
    }
}

#[cfg(target_os = "macos")]
pub(super) async fn cmd_desktop_drag(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let (pid, remaining) = parse_desktop_pid_and_rest_optional(args);
    let mut from = None;
    let mut to = None;
    let mut steps = 10u32;
    let mut i = 0;
    while i < remaining.len() {
        match remaining[i].as_str() {
            "--from" if i + 1 < remaining.len() => {
                from = Some(parse_xy(&remaining[i + 1])?);
                i += 2;
            }
            "--to" if i + 1 < remaining.len() => {
                to = Some(parse_xy(&remaining[i + 1])?);
                i += 2;
            }
            "--steps" if i + 1 < remaining.len() => {
                steps = remaining[i + 1].parse().unwrap_or(10);
                i += 2;
            }
            _ => i += 1,
        }
    }
    let (fx, fy) = from.context("--from x,y required")?;
    let (tx, ty) = to.context("--to x,y required")?;
    crate::desktop::bg_input::drag(fx, fy, tx, ty, pid, steps)?;
    out!(
        ctx,
        "{}",
        crate::output::to_string(&PlainOutput::new(format!(
            "Dragged ({fx:.0},{fy:.0}) → ({tx:.0},{ty:.0})"
        )))?
    );
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) async fn cmd_desktop_menubar(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "list" => {
            let items = crate::desktop::native::list_menubar_extras()?;
            out!(
                ctx,
                "{}",
                crate::output::to_string(&crate::desktop::DesktopMenubarListOutput { items })?
            );
            Ok(())
        }
        "click" => {
            let title = args
                .get(1)
                .filter(|a| *a != "click")
                .or_else(|| args.iter().find(|a| !a.starts_with("--")))
                .context("Usage: sidekar desktop menubar click <title>")?;
            let msg = crate::desktop::native::click_menubar_extra(title)?;
            out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
            Ok(())
        }
        other => bail!("Unknown menubar subcommand: {other}"),
    }
}

#[cfg(target_os = "macos")]
fn parse_xy(raw: &str) -> Result<(f64, f64)> {
    let (x, y) = raw.split_once(',').context("expected x,y coordinates")?;
    Ok((x.parse()?, y.parse()?))
}

#[cfg(target_os = "macos")]
pub(super) async fn cmd_desktop_type_extended(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let (pid, remaining) = parse_desktop_pid_and_rest_optional(args);
    let mut profile = "linear".to_string();
    let mut delay_ms = None;
    let mut wpm = None;
    let mut text_parts = Vec::new();
    let mut i = 0;
    while i < remaining.len() {
        match remaining[i].as_str() {
            "--profile" if i + 1 < remaining.len() => {
                profile = remaining[i + 1].clone();
                i += 2;
            }
            "--delay" if i + 1 < remaining.len() => {
                delay_ms = remaining[i + 1].parse().ok();
                i += 2;
            }
            "--wpm" if i + 1 < remaining.len() => {
                wpm = remaining[i + 1].parse().ok();
                i += 2;
            }
            other => {
                text_parts.push(other.to_string());
                i += 1;
            }
        }
    }
    let text = text_parts.join(" ");
    if text.is_empty() {
        bail!("Usage: sidekar desktop type [--profile human|linear] [--wpm N] [--delay MS] <text>");
    }
    let cadence = crate::desktop::typing::TypingProfile::from_args(&profile, delay_ms, wpm);
    crate::desktop::bg_input::type_with_profile(&text, cadence, pid)?;
    let target = pid.map(|p| format!(" → pid {p}")).unwrap_or_default();
    out!(
        ctx,
        "{}",
        crate::output::to_string(&PlainOutput::new(format!(
            "Typed {} chars ({profile}){target}",
            text.chars().count()
        )))?
    );
    Ok(())
}

#[cfg(not(target_os = "macos"))]
macro_rules! desktop_macos_only_stub {
    ($($name:ident),* $(,)?) => {
        $(
            pub(super) async fn $name(_ctx: &mut AppContext, _args: &[String]) -> Result<()> {
                bail!("Desktop automation is only available on macOS")
            }
        )*
    };
}

#[cfg(not(target_os = "macos"))]
desktop_macos_only_stub!(
    cmd_desktop_see,
    cmd_desktop_set_value,
    cmd_desktop_perform_action,
    cmd_desktop_menu_ext,
    cmd_desktop_dialog,
    cmd_desktop_window,
    cmd_desktop_space,
    cmd_desktop_drag,
    cmd_desktop_menubar,
    cmd_desktop_type_extended,
);
