//! One way to print a moment in the Slack and Linear commands.
//!
//! The commands are read mostly by agents, so a time is ISO 8601 in UTC with a
//! `Z`, to the second: `2026-09-14T03:36:49Z`. It is unambiguous, sorts as
//! text, and needs no knowledge of where the machine is. `--local` renders the
//! same moment in this machine's zone with an explicit offset
//! (`2026-09-13T23:36:49-04:00`) for a human reading along.
//!
//! What the APIs return in machine form (Slack `ts` ids, values sent back to
//! an API) is not reformatted; this is only for what is shown.

use chrono::{DateTime, Local, SecondsFormat, TimeZone, Utc};

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

impl Zone {
    /// `--local` anywhere in `args` selects local time. Returns the zone and
    /// the arguments without the flag, so it is accepted by every command.
    pub fn from_args(args: &[String]) -> (Self, Vec<String>) {
        let zone = if args.iter().any(|a| a == LOCAL_FLAG) {
            Zone::Local
        } else {
            Zone::Utc
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

/// A Slack message `ts` (`1700000000.123456`). Anything unparseable is shown
/// as given rather than hidden.
pub fn from_slack_ts(ts: &str, zone: Zone) -> String {
    match ts.split('.').next().and_then(|s| s.parse::<i64>().ok()) {
        Some(secs) => from_epoch(secs, zone),
        None => ts.to_string(),
    }
}

/// An ISO 8601 / RFC 3339 time as Linear returns it
/// (`2026-09-14T03:36:49.709Z`). Empty shows as empty; anything that does not
/// parse is shown as given.
pub fn from_iso(iso: &str, zone: Zone) -> String {
    let t = iso.trim();
    if t.is_empty() {
        return String::new();
    }
    match DateTime::parse_from_rfc3339(t) {
        Ok(at) => format(at.with_timezone(&Utc), zone),
        Err(_) => t.to_string(),
    }
}

#[cfg(test)]
mod tests;
