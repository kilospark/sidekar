use crate::*;

/// Capture a desktop screenshot using macOS screencapture CLI.
/// If pid is provided, captures the frontmost window of that app.
/// Returns Ok(()) after saving the PNG to output_path.
#[cfg(target_os = "macos")]
pub async fn capture_desktop_screenshot(pid: Option<i32>, output_path: &Path) -> Result<()> {
    use std::process::Command as StdCommand;

    let mut cmd = StdCommand::new("screencapture");
    cmd.arg("-x"); // no sound
    cmd.arg("-o"); // no shadow

    if let Some(pid) = pid {
        // Try to get a window ID for this pid via our Swift bridge
        let windows = crate::desktop::native::list_windows(pid)?;
        if let Some(win_id) = windows.first().and_then(|w| w.window_id) {
            cmd.arg("-l");
            cmd.arg(win_id.to_string());
        }
        // If no window ID found, falls through to full-screen capture
    }

    cmd.arg(output_path.to_string_lossy().as_ref());

    let status = cmd.status().context("failed to run screencapture")?;
    if !status.success() {
        bail!("screencapture exited with status {status}");
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub async fn capture_desktop_screenshot(_pid: Option<i32>, _output_path: &Path) -> Result<()> {
    bail!("Desktop screenshot is only available on macOS")
}
