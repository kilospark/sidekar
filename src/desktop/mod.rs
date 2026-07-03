#[cfg(target_os = "macos")]
pub mod bg_input;
#[cfg(target_os = "macos")]
pub mod focus_guard;
#[cfg(target_os = "macos")]
pub mod input;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use macos as native;
#[cfg(target_os = "macos")]
mod interact;
#[cfg(target_os = "macos")]
pub mod monitor;
#[cfg(target_os = "macos")]
pub mod refs;
#[cfg(target_os = "macos")]
pub mod screen;
#[cfg(target_os = "macos")]
pub mod skylight;
#[cfg(target_os = "macos")]
mod snapshot;
#[cfg(target_os = "macos")]
pub mod typing;
#[cfg(target_os = "macos")]
pub(crate) use snapshot::{build_see_snapshot, persist_snapshot};
#[cfg(target_os = "macos")]
pub(crate) mod spaces;
#[cfg(target_os = "macos")]
pub use interact::*;
#[cfg(target_os = "macos")]
pub use types::{DesktopMenubarListOutput, DesktopSpaceListOutput, DesktopWindowListOutput};
pub mod types;
