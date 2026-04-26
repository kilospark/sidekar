//! Clipboard helpers (macOS).
//!
//! Keyboard, mouse, and scroll input have moved to `bg_input.rs` which
//! uses CGEvent + SkyLight SPI for background-safe per-pid delivery.
//! This module retains only the clipboard operations that go through
//! pbcopy/pbpaste.

use crate::*;

#[cfg(target_os = "macos")]
pub fn set_clipboard_text(text: &str) -> Result<()> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to launch pbcopy")?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(text.as_bytes())
            .context("failed writing clipboard contents")?;
    }
    let output = child
        .wait_with_output()
        .context("failed waiting for pbcopy")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("pbcopy failed: {}", stderr.trim());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn read_clipboard_text() -> Result<String> {
    let output = Command::new("pbpaste")
        .output()
        .context("failed to run pbpaste")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("pbpaste failed: {}", stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
