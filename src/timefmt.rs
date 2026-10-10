//! One way to print a moment anywhere in the sidekar CLI.
//!
//! Output is read mostly by agents, so a time is ISO 8601 in UTC with a `Z`,
//! to the second: `2026-09-14T03:36:49Z`. It is unambiguous, sorts as text,
//! and needs no knowledge of where the machine is. `--local` renders the same
//! moment in this machine's zone with an explicit offset
//! (`2026-09-13T23:36:49-04:00`) for a human reading along.
//!
//! Only what is shown goes through here. Machine values (Slack `ts` ids,
//! values sent back to an API, stored rows, JSON fields) keep their own form.
//!
//! The zone is chosen once per command: `sidekar --local …` sets the process
//! default (see `main`), and a command group that also takes `--local` itself
//! (for callers inside one process: the REPL, agent tools, cron) runs its body
//! in [`scoped`], so concurrent commands do not see each other's choice.

use chrono::{DateTime, FixedOffset, Local, SecondsFormat, TimeZone, Utc};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};

/// Which zone shown times are rendered in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Zone {
    /// `2026-09-14T03:36:49Z`. The default.
    #[default]
    Utc,
    /// This machine's zone with its offset: `2026-09-13T23:36:49-04:00`.
    Local,
}

/// The flag that selects [`Zone::Local`].
pub const LOCAL_FLAG: &str = "--local";

static PROCESS_LOCAL: AtomicBool = AtomicBool::new(false);

tokio::task_local! {
    static SCOPED: Zone;
}

/// Set the zone for the whole process (`sidekar --local …`).
pub fn set_default(zone: Zone) {
    PROCESS_LOCAL.store(zone == Zone::Local, Ordering::Relaxed);
}

/// The zone shown times use right now: the command's own choice when it made
/// one, else the process default.
pub fn zone() -> Zone {
    SCOPED.try_with(|z| *z).unwrap_or_else(|_| {
        if PROCESS_LOCAL.load(Ordering::Relaxed) {
            Zone::Local
        } else {
            Zone::Utc
        }
    })
}

/// Run `fut` with shown times in `zone`.
pub async fn scoped<F: Future>(zone: Zone, fut: F) -> F::Output {
    SCOPED.scope(zone, fut).await
}

/// Run `f` with shown times in `zone`, for synchronous command bodies.
pub fn scoped_sync<R>(zone: Zone, f: impl FnOnce() -> R) -> R {
    SCOPED.sync_scope(zone, f)
}

/// Run a synchronous command body that prints times: `--local` is taken from
/// its args and its times are shown in the chosen zone.
pub fn run_sync<R>(args: &[String], f: impl FnOnce(&[String]) -> R) -> R {
    let (zone, rest) = Zone::from_args(args);
    scoped_sync(zone, || f(&rest))
}

impl Zone {
    /// `--local` anywhere in `args` selects local time; so does a `--local`
    /// given earlier (the process default or an enclosing command). Returns
    /// the zone and the arguments without the flag, so every command that
    /// prints a time accepts it.
    pub fn from_args(args: &[String]) -> (Self, Vec<String>) {
        let zone = if args.iter().any(|a| a == LOCAL_FLAG) {
            Zone::Local
        } else {
            zone()
        };
        let rest = args.iter().filter(|a| *a != LOCAL_FLAG).cloned().collect();
        (zone, rest)
    }
}

/// A moment as shown to the reader.
pub fn format(at: DateTime<Utc>, zone: Zone) -> String {
    match zone {
        Zone::Utc => at.to_rfc3339_opts(SecondsFormat::Secs, true),
        Zone::Local => at
            .with_timezone(&Local)
            .to_rfc3339_opts(SecondsFormat::Secs, false),
    }
}

/// Unix seconds. Zero or less is "no time" and shows as `-`.
pub fn from_epoch(secs: i64, zone: Zone) -> String {
    if secs <= 0 {
        return "-".into();
    }
    match Utc.timestamp_opt(secs, 0).single() {
        Some(at) => format(at, zone),
        None => secs.to_string(),
    }
}

/// Unix seconds that may carry a fraction (`1789357009.709`). The fraction is
/// dropped, as everywhere else.
pub fn from_epoch_f64(secs: f64, zone: Zone) -> String {
    if !secs.is_finite() {
        return "-".into();
    }
    from_epoch(secs.floor() as i64, zone)
}

/// Unix milliseconds.
pub fn from_epoch_ms(ms: i64, zone: Zone) -> String {
    if ms <= 0 {
        return "-".into();
    }
    from_epoch(ms.div_euclid(1000), zone)
}

/// A Slack message `ts` (`1700000000.123456`). Anything unparseable is shown
/// as given rather than hidden.
pub fn from_slack_ts(ts: &str, zone: Zone) -> String {
    match ts.split('.').next().and_then(|s| s.parse::<i64>().ok()) {
        Some(secs) => from_epoch(secs, zone),
        None => ts.to_string(),
    }
}

/// An ISO 8601 / RFC 3339 time as the APIs return it
/// (`2026-09-14T03:36:49.709Z`, `2026-09-13T23:36:49-04:00`). Empty shows as
/// empty; anything that does not parse is shown as given.
pub fn from_iso(iso: &str, zone: Zone) -> String {
    let t = iso.trim();
    if t.is_empty() {
        return String::new();
    }
    // RFC 3339 proper, or the same with a space for the `T` (as SQL
    // databases print it). A time with no zone at all is not guessed at.
    let parsed = DateTime::parse_from_rfc3339(t).or_else(|e| match t.get(10..11) {
        Some(" ") => DateTime::parse_from_rfc3339(&format!("{}T{}", &t[..10], &t[11..])),
        _ => Err(e),
    });
    match parsed {
        Ok(at) => format(at.with_timezone(&Utc), zone),
        Err(_) => t.to_string(),
    }
}

/// An email `Date:` header (RFC 2822 / 5322:
/// `Mon, 14 Sep 2026 05:36:49 +0200 (CEST)`). Empty shows as empty; anything
/// that does not parse is shown as given.
pub fn from_rfc2822(header: &str, zone: Zone) -> String {
    let t = header.trim();
    if t.is_empty() {
        return String::new();
    }
    match parse_rfc2822(t) {
        Some(at) => format(at.with_timezone(&Utc), zone),
        None => t.to_string(),
    }
}

fn parse_rfc2822(t: &str) -> Option<DateTime<FixedOffset>> {
    if let Ok(at) = DateTime::parse_from_rfc2822(t) {
        return Some(at);
    }
    // Mailers commonly append the zone's name as a comment: `+0200 (CEST)`.
    let bare = match t.rfind('(') {
        Some(i) if t.ends_with(')') => t[..i].trim_end(),
        _ => return None,
    };
    DateTime::parse_from_rfc2822(bare).ok()
}

/// A moment with how long ago it was, for lists read by a person:
/// `2026-09-14T03:36:49Z (3h ago)`. The absolute time comes first so the line
/// still means something when read later.
pub fn with_ago(secs: i64, now: i64, zone: Zone) -> String {
    if secs <= 0 {
        return "-".into();
    }
    format!("{} ({})", from_epoch(secs, zone), ago(now - secs))
}

/// `40s ago`, `3m ago`, `2h ago`, `5d ago`. A moment in the future (a clock
/// that moved) reads as `just now` rather than a negative age.
pub fn ago(elapsed_secs: i64) -> String {
    match elapsed_secs {
        s if s < 1 => "just now".into(),
        s if s < 60 => format!("{s}s ago"),
        s if s < 3_600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3_600),
        s => format!("{}d ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests;
