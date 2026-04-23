//! `/stats` slash command: live diagnostics for the REPL session.
//!
//! Produces a one-screen snapshot of:
//!   - Process RSS and CPU%, sampled via `ps` (portable enough across
//!     macOS and Linux, no extra crates). Fork+exec is ~5-10ms — fine
//!     for a manual command, not fine for a per-turn overlay.
//!   - Thread count, sampled the same way.
//!   - History length (# messages, estimated tokens). Lets the user
//!     see how close they are to the compaction threshold before
//!     the spinner tells them.
//!   - Editor input-history length, so growth regressions are
//!     visible (history is capped at 1000, display will never exceed).
//!
//! Intentionally minimal. If we need richer telemetry (per-tool call
//! counts, stream channel depth, cache hit rates), those deserve their
//! own subcommand rather than piling into /stats.

use crate::providers::ChatMessage;

/// Snapshot of process resource usage. All fields are best-effort —
/// any that we can't determine are reported as `None` so display
/// falls back gracefully rather than erroring out.
pub(super) struct ResourceSnapshot {
    /// Resident set size in KiB as reported by `ps`.
    pub rss_kib: Option<u64>,
    /// CPU percent as reported by `ps` (time-averaged since process
    /// start on macOS, instantaneous on Linux — so don't compare
    /// numbers directly across platforms).
    pub cpu_pct: Option<f32>,
    /// Number of threads in the process.
    pub threads: Option<u32>,
}

impl ResourceSnapshot {
    /// Sample via `ps -o rss=,pcpu=,nlwp= -p <pid>` (nlwp is Linux;
    /// we fall back to a second invocation for thread count on macOS
    /// only if needed). Kept in one file so the command surface is
    /// easy to audit — no hidden sampler tasks running in the
    /// background.
    pub(super) fn capture() -> Self {
        let pid = std::process::id();

        // Linux supports nlwp in a single ps call. macOS ps doesn't,
        // but `ps -M` gives per-thread rows we can count. Try the
        // Linux form first (fast path, single fork); on failure, do
        // the macOS path.
        #[cfg(target_os = "linux")]
        {
            let out = std::process::Command::new("ps")
                .args([
                    "-o",
                    "rss=,pcpu=,nlwp=",
                    "-p",
                    &pid.to_string(),
                ])
                .output();
            if let Ok(o) = out
                && o.status.success()
            {
                let text = String::from_utf8_lossy(&o.stdout);
                let mut it = text.split_whitespace();
                let rss_kib = it.next().and_then(|s| s.parse().ok());
                let cpu_pct = it.next().and_then(|s| s.parse().ok());
                let threads = it.next().and_then(|s| s.parse().ok());
                return Self {
                    rss_kib,
                    cpu_pct,
                    threads,
                };
            }
        }

        // macOS: rss + pcpu in one call, threads with -M.
        let rss_cpu = std::process::Command::new("ps")
            .args(["-o", "rss=,pcpu=", "-p", &pid.to_string()])
            .output();
        let mut rss_kib = None;
        let mut cpu_pct = None;
        if let Ok(o) = rss_cpu
            && o.status.success()
        {
            let text = String::from_utf8_lossy(&o.stdout);
            let mut it = text.split_whitespace();
            rss_kib = it.next().and_then(|s| s.parse().ok());
            cpu_pct = it.next().and_then(|s| s.parse().ok());
        }

        // Thread count via `ps -M -p <pid>` — header row + one row
        // per thread. Count lines - 1.
        let threads = std::process::Command::new("ps")
            .args(["-M", "-p", &pid.to_string()])
            .output()
            .ok()
            .and_then(|o| {
                if !o.status.success() {
                    return None;
                }
                let text = String::from_utf8_lossy(&o.stdout);
                let n = text.lines().count();
                if n > 1 {
                    Some((n - 1) as u32)
                } else {
                    None
                }
            });

        Self {
            rss_kib,
            cpu_pct,
            threads,
        }
    }
}

/// Formatted `/stats` text. Separated from I/O so unit tests can
/// assert the formatting without touching stdout or `ps`.
pub(super) fn format_stats(
    snap: &ResourceSnapshot,
    history: &[ChatMessage],
    editor_input_history_len: usize,
    model: &str,
    cred_name: &str,
    session_id: &str,
) -> String {
    let mut out = String::new();
    out.push_str("\x1b[1mSession\x1b[0m\n");
    out.push_str(&format!("  session   {session_id}\n"));
    out.push_str(&format!("  cred      {cred_name}\n"));
    out.push_str(&format!("  model     {model}\n"));
    out.push_str("\n\x1b[1mContext\x1b[0m\n");
    out.push_str(&format!("  messages  {}\n", history.len()));
    let tokens = crate::agent::compaction::estimate_tokens_public(history);
    out.push_str(&format!("  tokens    ~{}k (estimated)\n", tokens / 1000));
    out.push_str(&format!(
        "  inputs    {editor_input_history_len} lines (capped at 1000)\n"
    ));
    out.push_str("\n\x1b[1mProcess\x1b[0m\n");
    match snap.rss_kib {
        Some(kib) => out.push_str(&format!("  rss       {:.1} MiB\n", kib as f64 / 1024.0)),
        None => out.push_str("  rss       (unknown)\n"),
    }
    match snap.cpu_pct {
        Some(p) => out.push_str(&format!("  cpu       {p:.1}%\n")),
        None => out.push_str("  cpu       (unknown)\n"),
    }
    match snap.threads {
        Some(t) => out.push_str(&format!("  threads   {t}\n")),
        None => out.push_str("  threads   (unknown)\n"),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ContentBlock, Role};

    fn msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    #[test]
    fn format_handles_all_fields_present() {
        let snap = ResourceSnapshot {
            rss_kib: Some(32 * 1024),
            cpu_pct: Some(1.5),
            threads: Some(14),
        };
        let h = vec![msg("hi"), msg("there")];
        let s = format_stats(&snap, &h, 42, "claude-3-5-sonnet", "claude", "sess-1");
        assert!(s.contains("session   sess-1"));
        assert!(s.contains("cred      claude"));
        assert!(s.contains("model     claude-3-5-sonnet"));
        assert!(s.contains("messages  2"));
        assert!(s.contains("inputs    42 lines"));
        assert!(s.contains("rss       32.0 MiB"));
        assert!(s.contains("cpu       1.5%"));
        assert!(s.contains("threads   14"));
    }

    #[test]
    fn format_handles_missing_fields() {
        let snap = ResourceSnapshot {
            rss_kib: None,
            cpu_pct: None,
            threads: None,
        };
        let s = format_stats(&snap, &[], 0, "m", "c", "s");
        assert!(s.contains("rss       (unknown)"));
        assert!(s.contains("cpu       (unknown)"));
        assert!(s.contains("threads   (unknown)"));
        // Zero-message edge case should show tokens ~0k, not panic.
        assert!(s.contains("tokens    ~0k"));
    }
}
