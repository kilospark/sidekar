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

pub mod agent;
pub mod agent_cli;
pub mod cdp;
pub mod cdp_proxy;
pub mod cli;
pub mod code_intel;
pub mod commands;
pub mod config;
pub mod daemon;
pub mod desktop;
pub mod doc_intel;
pub mod events;
pub mod ext;
pub mod help;
pub mod md;
pub mod memory;
pub mod message;
pub mod pakt;
pub mod poller;
pub mod providers;
pub mod proxy;
pub mod pty;
pub mod repl;
pub mod repo;
pub mod rtk;
pub mod runtime;
pub mod scope;
pub mod scripts;
pub mod session;
pub mod skill;
pub mod tasks;
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
pub use cli::{
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
