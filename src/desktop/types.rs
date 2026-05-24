use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopAppInfo {
    pub pid: i32,
    pub bundle_id: Option<String>,
    pub name: String,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopWindowInfo {
    pub pid: i32,
    pub window_id: Option<u32>,
    pub title: Option<String>,
    pub frame: DesktopRect,
    pub is_main: bool,
    pub is_focused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopElementStep {
    pub role: String,
    pub title: Option<String>,
    pub identifier: Option<String>,
    pub index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopElementPath {
    pub pid: i32,
    pub chain: Vec<DesktopElementStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopElementMatch {
    pub path: DesktopElementPath,
    pub role: String,
    pub title: Option<String>,
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    pub frame: Option<DesktopRect>,
    pub actions: Vec<String>,
    /// Stable ref id (e.g. `@e3`), assigned by `find_elements`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopActionResult {
    pub action: String,
    pub role: String,
    pub title: Option<String>,
    pub method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSetValueResult {
    pub role: String,
    pub title: Option<String>,
    pub old_value: Option<String>,
    pub new_value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSeeSnapshot {
    pub snapshot_id: String,
    pub pid: i32,
    pub element_count: usize,
    pub screenshot_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotated_path: Option<String>,
    pub elements: Vec<DesktopElementMatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSpaceInfo {
    pub id: u64,
    pub index: usize,
    pub name: Option<String>,
    pub is_active: bool,
    pub space_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopDialogField {
    pub label: Option<String>,
    pub value: Option<String>,
    pub index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopDialogInfo {
    pub buttons: Vec<String>,
    pub fields: Vec<DesktopDialogField>,
    pub static_text: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopMenubarItem {
    pub title: String,
    pub index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<DesktopRect>,
}

#[cfg(target_os = "macos")]
mod command_output {
    use super::*;
    use crate::output::CommandOutput;
    use std::io::Write;

    macro_rules! impl_pretty_json_output {
        ($($ty:ty),+ $(,)?) => {$(
            impl CommandOutput for $ty {
                fn render_text(&self, w: &mut dyn Write) -> std::io::Result<()> {
                    let json = serde_json::to_string_pretty(self)
                        .map_err(|e| std::io::Error::other(e.to_string()))?;
                    writeln!(w, "{json}")
                }
            }
        )+};
    }

    impl_pretty_json_output!(
        DesktopSeeSnapshot,
        DesktopSetValueResult,
        DesktopActionResult,
        DesktopDialogInfo,
    );

    #[derive(Serialize)]
    pub struct DesktopWindowListOutput {
        pub windows: Vec<DesktopWindowInfo>,
    }

    impl CommandOutput for DesktopWindowListOutput {
        fn render_text(&self, w: &mut dyn Write) -> std::io::Result<()> {
            if self.windows.is_empty() {
                writeln!(w, "No windows found.")?;
                return Ok(());
            }
            for (i, win) in self.windows.iter().enumerate() {
                let title = win.title.as_deref().unwrap_or("(untitled)");
                writeln!(
                    w,
                    "  [{i}] \"{title}\" ({:.0},{:.0} {:.0}x{:.0})",
                    win.frame.x, win.frame.y, win.frame.width, win.frame.height
                )?;
            }
            Ok(())
        }
    }

    #[derive(Serialize)]
    pub struct DesktopSpaceListOutput {
        pub spaces: Vec<DesktopSpaceInfo>,
    }

    impl CommandOutput for DesktopSpaceListOutput {
        fn render_text(&self, w: &mut dyn Write) -> std::io::Result<()> {
            for space in &self.spaces {
                let name = space.name.as_deref().unwrap_or("(unnamed)");
                let active = if space.is_active { " (active)" } else { "" };
                writeln!(w, "  {}: {name}{active}", space.index)?;
            }
            Ok(())
        }
    }

    #[derive(Serialize)]
    pub struct DesktopMenubarListOutput {
        pub items: Vec<DesktopMenubarItem>,
    }

    impl CommandOutput for DesktopMenubarListOutput {
        fn render_text(&self, w: &mut dyn Write) -> std::io::Result<()> {
            if self.items.is_empty() {
                writeln!(w, "No menu bar extras found.")?;
                return Ok(());
            }
            for item in &self.items {
                writeln!(w, "  [{}] {}", item.index, item.title)?;
            }
            Ok(())
        }
    }
}

#[cfg(target_os = "macos")]
pub use command_output::{
    DesktopMenubarListOutput, DesktopSpaceListOutput, DesktopWindowListOutput,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopClickResult {
    pub kind: String,
    pub role: Option<String>,
    pub title: Option<String>,
    pub x: Option<f64>,
    pub y: Option<f64>,
}
