#[cfg(target_os = "macos")]
pub mod input;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use macos as native;
#[cfg(target_os = "macos")]
pub mod monitor;
#[cfg(target_os = "macos")]
pub mod screen;
pub mod types;
