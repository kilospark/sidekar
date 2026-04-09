use std::env;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

struct RuntimeState {
    verbose: AtomicBool,
    quiet: AtomicBool,
    pty_mode: bool,
    color: bool,
    agent_name: Mutex<Option<String>>,
    channel: Mutex<Option<String>>,
    cron_depth: AtomicUsize,
}

fn initial_var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn state() -> &'static RuntimeState {
    static RUNTIME: OnceLock<RuntimeState> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let no_color = env::var("NO_COLOR").is_ok();
        let is_tty = std::io::stdout().is_terminal();
        RuntimeState {
            verbose: AtomicBool::new(env::var("SIDEKAR_VERBOSE").is_ok()),
            quiet: AtomicBool::new(false),
            pty_mode: env::var("SIDEKAR_PTY").is_ok(),
            color: is_tty && !no_color,
            agent_name: Mutex::new(initial_var("SIDEKAR_AGENT_NAME")),
            channel: Mutex::new(initial_var("SIDEKAR_CHANNEL")),
            cron_depth: AtomicUsize::new(
                initial_var("SIDEKAR_CRON_DEPTH")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0),
            ),
        }
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

pub fn quiet() -> bool {
    state().quiet.load(Ordering::SeqCst)
}

pub fn set_quiet(value: bool) {
    state().quiet.store(value, Ordering::SeqCst);
}

pub fn color() -> bool {
    state().color
}

pub fn pty_mode() -> bool {
    state().pty_mode
}

/// Strip ANSI escape sequences from a string. Used to sanitize output when
/// stdout is not a terminal (piped to another process / file).
pub fn strip_ansi(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i < bytes.len() {
                match bytes[i] {
                    b'[' => {
                        i += 1;
                        while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                            i += 1;
                        }
                        if i < bytes.len() {
                            i += 1;
                        }
                    }
                    b']' => {
                        i += 1;
                        while i < bytes.len() {
                            if bytes[i] == 0x07 {
                                i += 1;
                                break;
                            }
                            if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                                i += 2;
                                break;
                            }
                            i += 1;
                        }
                    }
                    _ => {
                        i += 1;
                    }
                }
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Conditionally strip ANSI codes if color is disabled.
pub fn maybe_strip_ansi(input: &str) -> std::borrow::Cow<'_, str> {
    if color() {
        std::borrow::Cow::Borrowed(input)
    } else {
        std::borrow::Cow::Owned(strip_ansi(input))
    }
}

pub fn agent_name() -> Option<String> {
    state()
        .agent_name
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
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
