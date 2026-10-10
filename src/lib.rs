pub use anyhow::{Context, Result, anyhow, bail};
pub use base64::Engine;
pub use fs2::FileExt;
pub use futures_util::{SinkExt, StreamExt};
pub use rand::RngCore;
pub use reqwest::Client;
pub use serde_json::{Value, json};
pub use std::collections::{HashMap, HashSet, VecDeque};
pub use std::env;
pub use std::fmt::Write as _;
pub use std::fs;
pub use std::net::TcpListener;
pub use std::path::{Path, PathBuf};
pub use std::process::{Command, Stdio};
pub use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
pub use tokio::time::{sleep, timeout};
pub use tokio_tungstenite::tungstenite::protocol::Message;

static CDP_SEND_TIMEOUT_SECS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(60);

pub fn set_cdp_timeout_secs(secs: u64) {
    CDP_SEND_TIMEOUT_SECS.store(secs, std::sync::atomic::Ordering::SeqCst);
}

fn cdp_send_timeout() -> Duration {
    Duration::from_secs(CDP_SEND_TIMEOUT_SECS.load(std::sync::atomic::Ordering::SeqCst))
}

#[cfg(test)]
pub(crate) fn test_home_lock() -> &'static std::sync::Mutex<()> {
    use std::sync::{Mutex, OnceLock};

    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// A fresh directory under the system temp dir, removed when this drops,
/// whether the test passed or panicked.
///
/// Test fixtures go here rather than in a directory named after the process
/// id and removed at the end, if at all: those piled up by the hundred.
#[cfg(test)]
pub(crate) struct ScratchDir(std::path::PathBuf);

#[cfg(test)]
impl ScratchDir {
    /// A new directory named `sidekar-<label>-...`.
    pub(crate) fn new(label: &str) -> Self {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sidekar-{label}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        // A crashed run with the same pid can leave one behind.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Self(dir)
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.0
    }

    pub(crate) fn join(&self, name: impl AsRef<std::path::Path>) -> std::path::PathBuf {
        self.0.join(name)
    }
}

#[cfg(test)]
impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// HOME pointed at a fresh scratch directory for as long as this lives.
///
/// For a test that reaches the broker, or anything else under `~/.sidekar`,
/// without meaning to test HOME itself. Holds [`test_home_lock`], so it must
/// not be combined with a helper that takes the lock too.
#[cfg(test)]
pub(crate) struct ScratchHome {
    // Fields drop in order: the directory goes while the lock is still held.
    dir: ScratchDir,
    old: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl ScratchHome {
    pub(crate) fn new() -> Self {
        let lock = test_home_lock().lock().unwrap_or_else(|p| p.into_inner());
        let dir = ScratchDir::new("scratch-home");
        let old = std::env::var_os("HOME");
        // SAFETY: serialized by test_home_lock, restored on drop.
        unsafe { std::env::set_var("HOME", dir.path()) };
        Self {
            dir,
            old,
            _lock: lock,
        }
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        self.dir.path()
    }
}

#[cfg(test)]
impl Drop for ScratchHome {
    fn drop(&mut self) {
        match self.old.take() {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

#[cfg(test)]
mod tests;

const MAX_PENDING_EVENTS: usize = 1000;

#[macro_export]
macro_rules! out {
    ($ctx:expr, $($arg:tt)*) => {{
        use std::fmt::Write;
        let _ = writeln!($ctx.output, $($arg)*);
    }};
}

/// Structured warning log to stderr. Prefixed with "sidekar:" for grepability.
#[macro_export]
macro_rules! wlog {
    ($($arg:tt)*) => {{
        eprintln!("sidekar: {}", format!($($arg)*));
    }};
}

pub mod api_client;
pub mod app_context;
pub mod auth;
pub mod broker;
pub mod browser;
pub mod browser_session;
pub mod bus;

pub mod activity;
pub mod agent;
pub mod agent_cli;
pub mod cdp;
pub mod cdp_proxy;
pub mod cli;
pub mod command_catalog;
pub mod commands;
pub mod config;
pub mod daemon;
pub mod desktop;
pub mod doc_intel;
pub mod events;
pub mod ext;
pub mod google;
pub mod help;
pub mod help_text;
pub mod hosted;
pub mod http_client;
pub mod input_mode;
pub mod linear;
pub mod mcp;
pub mod md;
pub mod memory;
pub mod message;
pub mod oauth_loopback;
pub mod output;
pub mod pakt;
pub mod poller;
pub mod prompts;
pub mod providers;
pub mod proxy;
pub mod pty;
pub mod repl;
pub mod repo;
pub mod rtk;
pub mod runtime;
pub mod scope;
pub mod scripts;
pub mod secrets;
pub mod session;
pub mod skill;
pub mod slack;
pub mod tasks;
#[cfg(test)]
pub(crate) mod test_http;
pub mod transport;
pub mod tunnel;
pub mod types;
pub mod utils;

pub(crate) use app_context::atomic_write_json;
pub use app_context::{AppContext, sanitize_for_filename};
pub(crate) use browser::with_tab_locks_exclusive;
pub use browser::{
    InteractiveData, adopt_new_tabs, cache_key_from_url, check_js_error, check_tab_lock,
    clear_editable_element, diff_elements, editable_element_value, fetch_interactive_elements,
    focus_editable_element, get_frame_context_id, get_page_brief, load_action_cache,
    locate_element, locate_element_by_text, prepare_cdp, resolve_selector, runtime_evaluate,
    runtime_evaluate_with_context, save_action_cache, snapshot_tab_ids, type_text_verified,
    wait_for_network_idle, wait_for_ready_state_complete,
};
pub use browser_session::{BrowserSessionInfo, get_browser_session, list_browser_sessions};
pub use cdp::{
    CdpClient, DirectCdp, connect_to_tab, create_new_tab, create_new_window,
    detect_browser_from_port, get_debug_tabs, get_window_id_for_target, http_get_text,
    http_put_text, minimize_window_by_id, open_cdp, restore_window_by_id, verify_cdp_ready,
};
pub use command_catalog::{
    browser_ext_routable, browser_requires_session, browser_should_auto_launch,
    canonical_command_name, command_handler, command_requires_session,
    command_should_auto_launch_browser, is_ext_routable_command, is_known_command,
    removed_command_replacement,
};
pub use help::{print_command_help, print_help};
pub use scripts::*;
pub use types::*;
pub use utils::*;

pub const DEFAULT_CDP_PORT: u16 = 9222;
pub const DEFAULT_CDP_HOST: &str = "127.0.0.1";
pub const CACHE_TTL_MS: i64 = 48 * 60 * 60 * 1000;
pub const CACHE_MAX_ENTRIES: usize = 100;
