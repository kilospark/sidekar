use std::env;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

struct RuntimeState {
    verbose: AtomicBool,
    pty_mode: bool,
    agent_name: Mutex<Option<String>>,
    channel: Mutex<Option<String>>,
    cron_depth: AtomicUsize,
}

fn initial_var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn state() -> &'static RuntimeState {
    static RUNTIME: OnceLock<RuntimeState> = OnceLock::new();
    RUNTIME.get_or_init(|| RuntimeState {
        verbose: AtomicBool::new(env::var("SIDEKAR_VERBOSE").is_ok()),
        pty_mode: env::var("SIDEKAR_PTY").is_ok(),
        agent_name: Mutex::new(initial_var("SIDEKAR_AGENT_NAME")),
        channel: Mutex::new(initial_var("SIDEKAR_CHANNEL")),
        cron_depth: AtomicUsize::new(
            initial_var("SIDEKAR_CRON_DEPTH")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0),
        ),
    })
}

pub fn init(verbose_flag: bool) {
    let _ = state();
    if verbose_flag {
        set_verbose(true);
    } else {
        crate::providers::set_verbose(verbose());
    }
}

pub fn verbose() -> bool {
    state().verbose.load(Ordering::SeqCst)
}

pub fn set_verbose(value: bool) {
    state().verbose.store(value, Ordering::SeqCst);
    crate::providers::set_verbose(value);
}

pub fn pty_mode() -> bool {
    state().pty_mode
}

pub fn agent_name() -> Option<String> {
    state().agent_name.lock().ok().and_then(|guard| guard.clone())
}

pub fn set_agent_name(value: Option<String>) {
    if let Ok(mut guard) = state().agent_name.lock() {
        *guard = value.filter(|name| !name.is_empty());
    }
}

pub fn channel() -> Option<String> {
    state().channel.lock().ok().and_then(|guard| guard.clone())
}

pub fn set_channel(value: Option<String>) {
    if let Ok(mut guard) = state().channel.lock() {
        *guard = value.filter(|channel| !channel.is_empty());
    }
}

pub fn cron_depth() -> usize {
    state().cron_depth.load(Ordering::SeqCst)
}

pub struct CronActionGuard;

impl Drop for CronActionGuard {
    fn drop(&mut self) {
        state().cron_depth.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn enter_cron_action() -> CronActionGuard {
    state().cron_depth.fetch_add(1, Ordering::SeqCst);
    CronActionGuard
}
