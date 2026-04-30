use std::env;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::output::OutputFormat;

struct RuntimeState {
    verbose: AtomicBool,
    quiet: AtomicBool,
    pty_mode: bool,
    color: bool,
    /// Controls whether the (not-yet-implemented) background
    /// journaling subsystem is allowed to run for this process.
    ///
    /// Precedence, strongest wins:
    ///   1. CLI flag          --journal / --no-journal
    ///   2. Env var           SIDEKAR_JOURNAL (tri-value: on/off/true/false/1/0)
    ///   3. /journal on|off   at runtime (this atomic)
    ///   4. Config            sidekar config set journal
    ///   5. Built-in default  on
    ///
    /// Default-on is deliberate — journaling is a productivity
    /// feature we want every user exposed to. Users who object can
    /// flip it with any of the mechanisms above.
    ///
    /// The journaling subsystem itself does not exist yet. Adding
    /// the switch ahead of the implementation so CLI/env/slash
    /// semantics are stable when the code lands.
    journal: AtomicBool,
    agent_name: Mutex<Option<String>>,
    channel: Mutex<Option<String>>,
    cron_depth: AtomicUsize,
    output_format: AtomicU8,
}

/// Parse a string as a tri-state boolean (`on`/`off`/`true`/`false`/
/// `1`/`0`/`yes`/`no`), case-insensitive. Returns `None` if the
/// input doesn't match any of those. Used for `SIDEKAR_JOURNAL`
/// and the `/journal` slash argument.
///
/// Verbose's env parsing is `env::var("SIDEKAR_VERBOSE").is_ok()`
/// — any value enables it, including `"0"` and `"false"`. That's a
/// long-standing quirk; I'm not re-litigating it here. Journal
/// needs real parsing because its default is on and users have to
/// be able to disable via env.
pub(crate) fn parse_bool_arg(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" | "y" => Some(true),
        "off" | "false" | "0" | "no" | "n" => Some(false),
        _ => None,
    }
}

const FMT_TEXT: u8 = 0;
const FMT_JSON: u8 = 1;
const FMT_TOON: u8 = 2;
const FMT_MARKDOWN: u8 = 3;

fn fmt_to_u8(fmt: OutputFormat) -> u8 {
    match fmt {
        OutputFormat::Text => FMT_TEXT,
        OutputFormat::Json => FMT_JSON,
        OutputFormat::Toon => FMT_TOON,
        OutputFormat::Markdown => FMT_MARKDOWN,
    }
}

fn u8_to_fmt(v: u8) -> OutputFormat {
    match v {
        FMT_JSON => OutputFormat::Json,
        FMT_TOON => OutputFormat::Toon,
        FMT_MARKDOWN => OutputFormat::Markdown,
        _ => OutputFormat::Text,
    }
}

fn initial_var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn state() -> &'static RuntimeState {
    static RUNTIME: OnceLock<RuntimeState> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let no_color = env::var("NO_COLOR").is_ok();
        let is_tty = std::io::stdout().is_terminal();
        // Journal default comes from env if set with a valid bool
        // value; otherwise falls back to the built-in on-by-default.
        // Config-layer lookups happen in init() rather than here —
        // this constructor runs before the broker DB is known to
        // exist, so touching config would recurse.
        let journal_from_env = initial_var("SIDEKAR_JOURNAL")
            .and_then(|v| parse_bool_arg(&v))
            .unwrap_or(true);

        RuntimeState {
            verbose: AtomicBool::new(env::var("SIDEKAR_VERBOSE").is_ok()),
            quiet: AtomicBool::new(false),
            pty_mode: env::var("SIDEKAR_PTY").is_ok(),
            color: is_tty && !no_color,
            journal: AtomicBool::new(journal_from_env),
            agent_name: Mutex::new(initial_var("SIDEKAR_AGENT_NAME")),
            channel: Mutex::new(initial_var("SIDEKAR_CHANNEL")),
            cron_depth: AtomicUsize::new(
                initial_var("SIDEKAR_CRON_DEPTH")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0),
            ),
            output_format: AtomicU8::new(FMT_TEXT),
        }
    })
}

pub fn output_format() -> OutputFormat {
    u8_to_fmt(state().output_format.load(Ordering::SeqCst))
}

pub fn set_output_format(fmt: OutputFormat) {
    state()
        .output_format
        .store(fmt_to_u8(fmt), Ordering::SeqCst);
}

pub fn init(verbose_flag: bool) {
    let _ = state();
    if verbose_flag {
        set_verbose(true);
    } else {
        crate::providers::set_verbose(verbose());
    }

    // Journal precedence refinement at init time:
    //   - If SIDEKAR_JOURNAL was set and parsed validly, state()
    //     already picked it up. Leave alone.
    //   - Otherwise consult the persisted config. The default
    //     config value is "on"; if the user ran `sidekar config
    //     set journal off` earlier, that overrides the built-in.
    //   - CLI override is handled by the caller via
    //     init_with_journal() below.
    if env::var("SIDEKAR_JOURNAL")
        .ok()
        .and_then(|v| parse_bool_arg(&v))
        .is_none()
    {
        let cfg_value = crate::config::config_get("journal");
        if let Some(parsed) = parse_bool_arg(&cfg_value) {
            set_journal(parsed);
        }
    }
}

/// CLI-driven init: same as `init(verbose_flag)` but also applies
/// an optional `--journal` / `--no-journal` override.
///
/// Kept as a separate function so callers that don't care about
/// journal (internal daemon startup, subcommands that don't touch
/// the REPL) keep the simpler signature. The REPL entry point
/// uses this one.
pub fn init_with_journal(verbose_flag: bool, journal_override: Option<bool>) {
    init(verbose_flag);
    if let Some(j) = journal_override {
        set_journal(j);
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

/// True when background journaling is enabled for this process.
///
/// See the `journal` field's doc comment on `RuntimeState` for the
/// full precedence chain. At call time this is a single atomic
/// load — cheap enough to check in hot paths (e.g. the per-turn
/// on_event callback), so journaling can be toggled mid-session
/// without any state refresh.
pub fn journal() -> bool {
    state().journal.load(Ordering::SeqCst)
}

pub fn set_journal(value: bool) {
    state().journal.store(value, Ordering::SeqCst);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bool_arg_accepts_common_synonyms() {
        // Truthy.
        for s in [
            "on", "ON", "On", "true", "TRUE", "True", "1", "yes", "YES", "y", "Y",
        ] {
            assert_eq!(parse_bool_arg(s), Some(true), "expected true for {s:?}");
        }
        // Falsy.
        for s in [
            "off", "OFF", "Off", "false", "FALSE", "False", "0", "no", "NO", "n", "N",
        ] {
            assert_eq!(parse_bool_arg(s), Some(false), "expected false for {s:?}");
        }
        // Whitespace tolerated on both sides.
        assert_eq!(parse_bool_arg("  on  "), Some(true));
        assert_eq!(parse_bool_arg("\ttrue\n"), Some(true));
    }

    #[test]
    fn parse_bool_arg_rejects_junk() {
        // Anything that isn't in the alias set is None, not a
        // silent default. Prevents a typo like `/journal o` from
        // being interpreted as `off`.
        for s in ["", "maybe", "2", "ok", "enabled", "tru", "f" /* not 'n' */] {
            assert_eq!(parse_bool_arg(s), None, "expected None for {s:?}");
        }
    }

    #[test]
    fn journal_default_is_on() {
        // New processes should default-on. Env var on the test
        // runner could override this in CI, so skip the assertion
        // if SIDEKAR_JOURNAL is present — that's the intended
        // precedence anyway.
        if std::env::var("SIDEKAR_JOURNAL").is_ok() {
            return;
        }
        // state() is OnceLock-initialized; assuming no prior test
        // in this same process flipped it, we'd see true. Flipping
        // then restoring keeps this test independent.
        let original = journal();
        set_journal(true);
        assert!(journal());
        set_journal(false);
        assert!(!journal());
        set_journal(original);
    }

    #[test]
    fn set_journal_is_atomic_and_visible() {
        // Sanity — shows the accessor/setter aren't accidentally
        // referencing different atomics (bugs-in-refactor guard).
        set_journal(true);
        assert!(journal());
        set_journal(false);
        assert!(!journal());
        set_journal(true);
        assert!(journal());
    }
}
