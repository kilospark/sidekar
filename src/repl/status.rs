//! `/status` slash command: user-facing session/model/usage snapshot.
//!
//! Overlaps with `/stats` but targets a different audience:
//!
//!   - `/stats` is diagnostic: RSS, CPU, thread count, editor history
//!     length. Useful when debugging sidekar itself or chasing a
//!     memory/typing-lag regression.
//!   - `/status` is operational: which model, which cred, how many
//!     tokens burned this session, how close to the compaction
//!     threshold, what was the last response_id. Useful when
//!     deciding whether to keep going or start a new session.
//!
//! Both exist. Neither is renamed. Users asking "am I about to
//! auto-compact?" should run `/status`; users asking "is sidekar
//! leaking memory?" should run `/stats`.
//!
//! Pure formatter. No I/O, no mutex. All data comes through the
//! `StatusView` argument so unit tests can assert layout without
//! threading `Arc<Mutex<TurnStats>>`.

use crate::providers::{StopReason, Usage};

/// All the inputs `/status` needs, pre-extracted from REPL state so
/// this module is easy to test and has no coupling to the REPL's
/// mutable state or the renderer's mutex.
pub(super) struct StatusView<'a> {
    pub session_id: &'a str,
    pub cwd: &'a str,
    pub model: &'a str,
    pub cred_name: &'a str,
    /// Context window size in tokens, if known (the model has been
    /// queried during this process). None when the REPL hasn't run
    /// a turn on this model yet — we still render /status, just
    /// without the progress bar.
    pub context_window: Option<u32>,
    /// Rough estimate of current history token count. Same heuristic
    /// as `/stats` and `maybe_compact`. In tokens, not kilo-tokens —
    /// formatting happens here.
    pub history_tokens_estimate: usize,
    /// Message count in current history (for a quick sanity figure
    /// next to the token estimate).
    pub history_messages: usize,
    /// Cumulative usage across all turns in this session.
    pub cumulative: &'a Usage,
    /// Count of turns (Done events) observed. 0 before first turn.
    pub turn_count: u32,
    /// Most recent turn's usage, if any turn has completed.
    pub last: Option<&'a Usage>,
    /// Most recent turn's stop reason, if any turn has completed.
    pub last_stop_reason: Option<&'a StopReason>,
    /// Most recent turn's response id. Empty for providers without
    /// one (most non-Codex) or before any turn.
    pub last_response_id: &'a str,
    /// Time since session start, for "up 2h 14m" display.
    pub session_age: std::time::Duration,
    /// Time since last turn completed, if any turn has. None before
    /// first turn.
    pub since_last_turn: Option<std::time::Duration>,
    /// Whether background journaling is currently enabled. Plumbed
    /// through the view (rather than read directly from
    /// `crate::runtime::journal()` inside the formatter) so this
    /// function stays pure and unit-testable without touching any
    /// global state.
    pub journal_on: bool,
    /// Remaining local lockout duration for current credential, if any.
    pub credential_lock_remaining: Option<std::time::Duration>,
}

/// Same 90% rule as `agent::compaction::maybe_compact`. Mirrored here
/// (rather than imported) because this is a display-only value; we
/// want the two to change together deliberately, not have /status
/// silently shift because someone tuned compaction.
const COMPACT_THRESHOLD_FRACTION: (u32, u32) = (9, 10);

/// Fixed-width progress bar for context-window fill. 40 cells wide —
/// wide enough to show meaningful motion, narrow enough to fit under
/// 80-col terminals alongside the percentage text.
const BAR_WIDTH: usize = 40;

/// Format a u32 token count with comma thousands separators, ASCII
/// only so it renders identically regardless of terminal locale.
fn commas(n: u32) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        let remaining = bytes.len() - i;
        out.push(*b as char);
        if remaining > 1 && remaining % 3 == 1 {
            out.push(',');
        }
    }
    out
}

/// Format a Duration as the largest reasonable unit ("2h 14m",
/// "45s", "3d 2h"). Same vibe as `session::format_relative_age`
/// but without the "ago" suffix — caller adds context.
fn fmt_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        return format!("{}m {}s", secs / 60, secs % 60);
    }
    if secs < 86400 {
        return format!("{}h {}m", secs / 3600, (secs % 3600) / 60);
    }
    format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
}

/// Render a context-window fill bar. `frac` is clamped to [0, 1].
/// Cells up to the threshold fraction are green; cells past it are
/// yellow (pre-compact) or red (post-compact, but we should have
/// already compacted by then — red is mostly a "you shouldn't see
/// this" signal).
fn bar(frac: f64, threshold_frac: f64) -> String {
    let f = frac.clamp(0.0, 1.0);
    let filled = (f * BAR_WIDTH as f64).round() as usize;
    let threshold_cell = (threshold_frac * BAR_WIDTH as f64).round() as usize;
    let mut out = String::with_capacity(BAR_WIDTH * 4 + 16);
    out.push('[');
    for i in 0..BAR_WIDTH {
        if i < filled {
            // Green below threshold, yellow approaching, red past.
            if i < threshold_cell {
                out.push_str("\x1b[32m█\x1b[0m");
            } else {
                out.push_str("\x1b[31m█\x1b[0m");
            }
        } else {
            out.push_str("\x1b[2m░\x1b[0m");
        }
    }
    out.push(']');
    out
}

fn fmt_stop_reason(s: &StopReason) -> &'static str {
    match s {
        StopReason::Stop => "Stop",
        StopReason::Length => "Length (hit max_tokens)",
        StopReason::ToolUse => "ToolUse",
        StopReason::Error => "Error",
        StopReason::Aborted => "Aborted",
    }
}

/// Produce the full `/status` output. Pure function — caller prints.
pub(super) fn format_status(v: &StatusView<'_>) -> String {
    let mut out = String::new();

    // ----- Session --------------------------------------------------
    out.push_str("\x1b[1mSession\x1b[0m\n");
    out.push_str(&format!("  id        {}\n", v.session_id));
    out.push_str(&format!("  cwd       {}\n", v.cwd));
    out.push_str(&format!("  up        {}\n", fmt_duration(v.session_age)));
    out.push_str(&format!("  turns     {}\n", v.turn_count));
    out.push_str(&format!("  messages  {}\n", v.history_messages));

    // ----- Model ----------------------------------------------------
    out.push_str("\n\x1b[1mModel\x1b[0m\n");
    out.push_str(&format!("  name      {}\n", v.model));
    out.push_str(&format!("  cred      {}\n", v.cred_name));
    match v.context_window {
        Some(cw) => out.push_str(&format!("  context   {} tokens\n", commas(cw))),
        None => out.push_str("  context   unknown (no turn yet)\n"),
    }
    // Session-scoped runtime toggles so users can see at a glance
    // what background features are live without running `/journal` /
    // `/relay` one at a time. Sourced from the view, not from
    // runtime globals, so the formatter stays pure.
    out.push_str(&format!(
        "  journal   {}\n",
        if v.journal_on { "on" } else { "off" }
    ));
    if let Some(remaining) = v.credential_lock_remaining {
        out.push_str(&format!(
            "  warning   \x1b[31mrate-limited; resets in {}\x1b[0m\n",
            fmt_duration(remaining)
        ));
    }

    // ----- Usage (cumulative) --------------------------------------
    out.push_str("\n\x1b[1mUsage (this session)\x1b[0m\n");
    if v.turn_count == 0 {
        out.push_str("  \x1b[2mNo turns completed yet.\x1b[0m\n");
    } else {
        out.push_str(&format!(
            "  input         {:>10}\n",
            commas(v.cumulative.input_tokens)
        ));
        out.push_str(&format!(
            "  output        {:>10}\n",
            commas(v.cumulative.output_tokens)
        ));
        // Only show cache rows when the provider reported any cache
        // activity. Avoids cluttering the grok/gpt cases where cache
        // is permanently zero.
        if v.cumulative.cache_read_tokens > 0 || v.cumulative.cache_write_tokens > 0 {
            out.push_str(&format!(
                "  cache read    {:>10}\n",
                commas(v.cumulative.cache_read_tokens)
            ));
            out.push_str(&format!(
                "  cache write   {:>10}\n",
                commas(v.cumulative.cache_write_tokens)
            ));
            // Hit rate = reads / (reads + writes). Intuitive enough
            // without overclaiming — the "miss" ratio includes first
            // writes that'll pay off on subsequent turns.
            let denom = v.cumulative.cache_read_tokens + v.cumulative.cache_write_tokens;
            if denom > 0 {
                let pct = v.cumulative.cache_read_tokens as f64 * 100.0 / denom as f64;
                out.push_str(&format!("  cache hit     {pct:>9.1}%\n"));
            }
        }
        out.push_str(&format!(
            "  \x1b[1mtotal         {:>10}\x1b[0m\n",
            commas(v.cumulative.total_tokens())
        ));
    }

    // ----- Last turn -----------------------------------------------
    if let Some(last) = v.last {
        out.push_str("\n\x1b[1mLast turn\x1b[0m\n");
        out.push_str(&format!(
            "  input         {:>10}\n",
            commas(last.input_tokens)
        ));
        out.push_str(&format!(
            "  output        {:>10}\n",
            commas(last.output_tokens)
        ));
        if last.cache_read_tokens > 0 || last.cache_write_tokens > 0 {
            out.push_str(&format!(
                "  cache read    {:>10}\n",
                commas(last.cache_read_tokens)
            ));
            out.push_str(&format!(
                "  cache write   {:>10}\n",
                commas(last.cache_write_tokens)
            ));
        }
        if let Some(sr) = v.last_stop_reason {
            // Only highlight non-normal stops — Stop/ToolUse are
            // routine and don't deserve a dim row reminding you they
            // happened.
            match sr {
                StopReason::Stop | StopReason::ToolUse => {
                    out.push_str(&format!("  end reason    {}\n", fmt_stop_reason(sr)));
                }
                _ => {
                    out.push_str(&format!(
                        "  end reason    \x1b[33m{}\x1b[0m\n",
                        fmt_stop_reason(sr)
                    ));
                }
            }
        }
        if !v.last_response_id.is_empty() {
            out.push_str(&format!("  response_id   {}\n", v.last_response_id));
        }
        if let Some(since) = v.since_last_turn {
            out.push_str(&format!(
                "  \x1b[2mcompleted {} ago\x1b[0m\n",
                fmt_duration(since)
            ));
        }
    }

    // ----- Context-window bar --------------------------------------
    // Only draw the bar when we know the window size AND we have an
    // estimate to compare it to. `history_tokens_estimate` is a rough
    // char/4 proxy; it'll be 10-20% off the real count the provider
    // sees, but good enough to show "you're about to compact".
    if let Some(cw) = v.context_window
        && cw > 0
    {
        let (num, den) = COMPACT_THRESHOLD_FRACTION;
        let threshold_tokens = (cw as u64 * num as u64 / den as u64) as u32;
        let used = v.history_tokens_estimate as u32;
        let frac = used as f64 / cw as f64;
        let threshold_frac = num as f64 / den as f64;

        out.push_str("\n\x1b[1mContext window\x1b[0m\n");
        out.push_str(&format!(
            "  {}  {:.1}% ({} / {})\n",
            bar(frac, threshold_frac),
            frac * 100.0,
            commas(used),
            commas(cw)
        ));
        out.push_str(&format!(
            "  \x1b[2mcompact at {}% — {} tokens\x1b[0m\n",
            num * 100 / den,
            commas(threshold_tokens)
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_defaults<'a>(cum: &'a Usage, turns: u32, last: Option<&'a Usage>) -> StatusView<'a> {
        StatusView {
            session_id: "sess",
            cwd: "/tmp",
            model: "claude-3-5-sonnet",
            cred_name: "claude",
            context_window: Some(200_000),
            history_tokens_estimate: 12_345,
            history_messages: 8,
            cumulative: cum,
            turn_count: turns,
            last,
            last_stop_reason: last.map(|_| &StopReason::Stop),
            last_response_id: "",
            session_age: std::time::Duration::from_secs(125),
            since_last_turn: last.map(|_| std::time::Duration::from_secs(5)),
            journal_on: true,
            credential_lock_remaining: None,
        }
    }

    #[test]
    fn commas_formats_thousands() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(42), "42");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1_000), "1,000");
        assert_eq!(commas(12_345), "12,345");
        assert_eq!(commas(1_000_000), "1,000,000");
        assert_eq!(commas(u32::MAX), "4,294,967,295");
    }

    #[test]
    fn fmt_duration_picks_coarsest_unit() {
        use std::time::Duration;
        assert_eq!(fmt_duration(Duration::from_secs(5)), "5s");
        assert_eq!(fmt_duration(Duration::from_secs(90)), "1m 30s");
        assert_eq!(fmt_duration(Duration::from_secs(3700)), "1h 1m");
        assert_eq!(fmt_duration(Duration::from_secs(90_000)), "1d 1h");
    }

    #[test]
    fn empty_session_shows_placeholder_no_panic() {
        let cum = Usage::default();
        let v = view_defaults(&cum, 0, None);
        let s = format_status(&v);
        assert!(s.contains("No turns completed yet"));
        // Last turn section absent when turn_count is 0.
        assert!(!s.contains("Last turn"));
        // Still shows the session/model blocks.
        assert!(s.contains("session\n") || s.contains("Session"));
        assert!(s.contains("claude-3-5-sonnet"));
    }

    #[test]
    fn populated_session_shows_all_blocks() {
        let cum = Usage {
            input_tokens: 10_000,
            output_tokens: 2_000,
            cache_read_tokens: 5_000,
            cache_write_tokens: 1_000,
        };
        let last = Usage {
            input_tokens: 1_200,
            output_tokens: 340,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let v = view_defaults(&cum, 3, Some(&last));
        let s = format_status(&v);
        assert!(s.contains("turns     3"));
        assert!(s.contains("input         "));
        assert!(s.contains("10,000"));
        assert!(s.contains("cache read"));
        assert!(s.contains("cache hit"));
        assert!(s.contains("Last turn"));
        assert!(s.contains("1,200"));
        // Cache rows for Last turn suppressed when zero.
        assert!(!s.contains("cache read           0"));
        assert!(s.contains("Context window"));
        // Bar is drawn (looks for the escape code prefix; exact cell
        // count depends on the fraction).
        assert!(s.contains("\x1b[32m█") || s.contains("\x1b[2m░"));
    }

    #[test]
    fn unknown_context_suppresses_bar() {
        let cum = Usage::default();
        let mut v = view_defaults(&cum, 0, None);
        v.context_window = None;
        let s = format_status(&v);
        assert!(s.contains("unknown (no turn yet)"));
        assert!(!s.contains("Context window"));
    }

    /// Not asserted — run with `cargo test preview_output -- --nocapture`
    /// to eyeball the full /status render. Useful when tuning layout
    /// without launching the REPL.
    #[test]
    fn preview_output() {
        let cum = Usage {
            input_tokens: 81_234,
            output_tokens: 5_120,
            cache_read_tokens: 62_110,
            cache_write_tokens: 4_480,
        };
        let last = Usage {
            input_tokens: 12_340,
            output_tokens: 890,
            cache_read_tokens: 11_800,
            cache_write_tokens: 0,
        };
        let v = StatusView {
            session_id: "1e420e8f",
            cwd: "/Users/karthik/src/sidekar",
            model: "gemini-2.5-pro",
            cred_name: "gem (Gemini)",
            context_window: Some(1_048_576),
            history_tokens_estimate: 90_834,
            history_messages: 14,
            cumulative: &cum,
            turn_count: 7,
            last: Some(&last),
            last_stop_reason: Some(&StopReason::Stop),
            last_response_id: "resp_abc123",
            session_age: std::time::Duration::from_secs(2 * 3600 + 14 * 60),
            since_last_turn: Some(std::time::Duration::from_secs(12)),
            journal_on: true,
            credential_lock_remaining: None,
        };
        let s = format_status(&v);
        // Eye-visible only when --nocapture is set.
        eprintln!("\n{s}");
    }

    #[test]
    fn abnormal_stop_is_highlighted() {
        let cum = Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let last = cum.clone();
        let mut v = view_defaults(&cum, 1, Some(&last));
        v.last_stop_reason = Some(&StopReason::Length);
        let s = format_status(&v);
        // Length stop is wrapped in yellow ANSI.
        assert!(s.contains("\x1b[33mLength"));
        // Stop (normal) would not be wrapped.
        v.last_stop_reason = Some(&StopReason::Stop);
        let s2 = format_status(&v);
        assert!(!s2.contains("\x1b[33mStop"));
    }

    #[test]
    fn rate_limited_credential_renders_warning() {
        let cum = Usage::default();
        let mut v = view_defaults(&cum, 0, None);
        v.credential_lock_remaining = Some(std::time::Duration::from_secs(95));
        let s = format_status(&v);
        assert!(s.contains("rate-limited"));
        assert!(s.contains("1m 35s"));
    }
}
