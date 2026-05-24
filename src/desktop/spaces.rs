//! macOS Spaces: list/switch/move-window (best-effort via Mission Control shortcuts).

use super::types::DesktopSpaceInfo;
use anyhow::{Result, bail};

const SPACE_KEYS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

pub fn list_spaces() -> Result<Vec<DesktopSpaceInfo>> {
    // macOS does not expose a stable public Spaces API. We expose the Ctrl+1..9
    // shortcuts Sidekar can drive, matching the default Mission Control layout.
    Ok((1..=9)
        .map(|i| DesktopSpaceInfo {
            id: i as u64,
            index: i,
            name: Some(format!("Desktop {i}")),
            is_active: i == 1,
            space_type: "user".into(),
        })
        .collect())
}

pub fn switch_space(index: usize) -> Result<String> {
    if index == 0 || index > 9 {
        bail!("space index must be 1-9");
    }
    let key = SPACE_KEYS[index - 1];
    super::bg_input::hotkey(&["ctrl", key], None)?;
    Ok(format!("Switched to space {index} (Ctrl+{key})"))
}

pub fn move_window_to_space(pid: i32, window_index: usize, space_index: usize) -> Result<String> {
    if space_index == 0 || space_index > 9 {
        bail!("space index must be 1-9");
    }
    super::macos::activate_app(pid)?;
    // Ctrl+number assigns the window to that space when dragging; simulate via
    // window menu is unreliable. Use Ctrl+Shift+number (assign) isn't standard.
    // Best-effort: focus window then switch with follow.
    let window = super::macos::window_element_at(pid, window_index)?;
    super::macos::raise_window(window)?;
    super::macos::release_ax_element(window);
    super::bg_input::hotkey(&["ctrl", SPACE_KEYS[space_index - 1]], None)?;
    Ok(format!(
        "Raised window {window_index} on pid {pid} and switched to space {space_index}"
    ))
}
