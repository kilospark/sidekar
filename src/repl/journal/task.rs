//! The journaling background task — the orchestrator that turns
//! an idle REPL session into a persisted summary.
//!
//! Responsibilities, in order:
//!   1. Gate on `runtime::journal()` — bail early if disabled.
//!   2. Check the per-project token budget via
//!      `store::project_tokens_in_window`. Bail if over cap.
//!   3. Load the slice of history after the previous journal's
//!      upper bound. Bail if empty.
//!   4. Run the pre-filter. Bail on `Verdict::Skip`.
//!   5. Redact credentials from the slice in place.
//!   6. Format the prompt (fresh or iterative depending on
//!      whether a previous journal exists).
//!   7. Call the LLM. The active session's provider+model are
//!      the default; env overrides are `SIDEKAR_JOURNAL_MODEL`
//!      and `SIDEKAR_JOURNAL_PROVIDER` (advisory — must already
//!      be configured).
//!   8. Parse the response defensively. Skip-persist on hard
//!      parse failure when there's no previous journal to fall
//!      back to; otherwise record the degraded version so the
//!      iterative chain can recover next pass.
//!   9. Run the threat scanner on every field. Replace hits
//!      with `[blocked]` sentinel.
//!  10. Insert the row. Update the idle tracker via
//!      `record_fired()` so we don't double-fire in the same
//!      idle window.
//!
//! Design notes:
//!   - Runs on a detached tokio task spawned by `repl.rs`.
//!     Holds no references into the REPL's mutable state: it
//!     owns an Arc<Provider>, a copy of session_id/project/
//!     model/cred_name as String, and an Arc<IdleTracker>.
//!     History is reloaded from the DB on each pass rather
//!     than snapshotted, because the REPL mutates `history`
//!     concurrently.
//!   - LLM call is non-streaming from our point of view — we
//!     drain the stream into a string. Tools are empty.
//!   - Token accounting: we record `tokens_in` / `tokens_out`
//!     as *estimated* values (chars / 4, rough heuristic). The
//!     cap is a soft guardrail, not a billing line — exact
//!     accounting would require provider-specific usage
//!     plumbing we don't want to maintain for a journal.
//!
//! This module is *not* `#[allow(dead_code)]`: everything in it
//! has an immediate caller, either internal or in `repl.rs`.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};

use crate::broker;
use crate::providers::{ChatMessage, ContentBlock, Provider, Role, StreamEvent};
use crate::runtime;

use super::idle::IdleTracker;
use super::parse;
use super::prefilter::{self, Verdict};
use super::promote;
use super::prompt;
use super::redact;
use super::scan;
use super::store::{self, JournalInsert};

/// Per-project soft cap on input tokens spent on journaling in a
/// 24-hour rolling window. Tuned to the worst case of "1 summary
/// every 90s for 8 hours of continuous REPL use" with typical
/// slice size — ~10k tokens covers that with margin. Users hit
/// this cap only in genuine abuse; normal use lands well below.
const DAILY_PROJECT_TOKEN_CAP: i64 = 10_000;
const DAILY_WINDOW_SECS: f64 = 24.0 * 3600.0;

/// Polling interval for the idle-check loop. Tuned for
/// responsiveness (journal fires within this + threshold of true
/// idleness) without wasting cycles. 5s is a round number that's
/// imperceptibly fast given a default 90s idle threshold.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Default idle threshold. Overridable via `SIDEKAR_JOURNAL_IDLE_SECS`.
const DEFAULT_IDLE_SECS: u64 = 90;

/// Bundle of inputs a journaling pass needs. Passed by value into
/// `run_once` so the function doesn't need lifetimes on every
/// field; all strings are owned copies.
#[derive(Clone)]
pub(crate) struct Context {
    pub provider: Arc<Provider>,
    pub session_id: String,
    pub project: String,
    /// Active session's primary model — used by default as the
    /// summarizer model too. A dedicated `SIDEKAR_JOURNAL_MODEL`
    /// env overrides this.
    pub model: String,
    /// Active credential name (as stored in sidekar's credential
    /// vault). Recorded on the journal row for provenance only;
    /// the provider already has the resolved key baked in.
    pub cred_name: String,
}

/// Outcome enum for `run_once`. Separate variants for each skip
/// reason so the caller (or tests) can distinguish "no history"
/// from "over budget" from "LLM failed." Inserted ids and token
/// counts ride along on `Persisted`.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum Outcome {
    Persisted {
        id: i64,
        tokens_in: i64,
        tokens_out: i64,
        degraded: bool,
        threat_labels: Vec<&'static str>,
    },
    SkippedJournalOff,
    SkippedOverBudget {
        spent: i64,
        cap: i64,
    },
    SkippedEmptySlice,
    SkippedLowSignal {
        reason: &'static str,
    },
    /// A hard failure downstream of all skip gates. Propagated
    /// to the caller for logging; the idle tracker is still
    /// marked fired to prevent a tight retry loop on a recurring
    /// failure (a subsequent turn's Done event re-arms).
    Failed(anyhow::Error),
}

/// One full journaling pass. Called by the polling loop in
/// `spawn_polling_loop` when `should_fire()` goes true. Also
/// callable directly from `/journal now` (step 10).
pub(crate) async fn run_once(ctx: &Context) -> Outcome {
    if !runtime::journal() {
        return Outcome::SkippedJournalOff;
    }

    // Budget gate. Conservative: check *before* we spend any
    // tokens. A concurrent invocation could sneak an insert in
    // between the read and our own insert, slightly overshooting
    // the cap — we don't lock for that because the cap is already
    // a soft guardrail, and the extra complexity isn't worth
    // preventing a few-hundred-token overshoot.
    let since = now_unix_secs() - DAILY_WINDOW_SECS;
    match store::project_tokens_in_window(&ctx.project, since) {
        Ok(spent) if spent >= DAILY_PROJECT_TOKEN_CAP => {
            return Outcome::SkippedOverBudget {
                spent,
                cap: DAILY_PROJECT_TOKEN_CAP,
            };
        }
        Ok(_) => {}
        Err(e) => {
            // DB error reading the budget isn't fatal — fall through,
            // the insert at the end will surface the same issue if
            // the problem is persistent. Log and continue.
            broker::try_log_error("journal", &format!("budget read failed: {e:#}"), None);
        }
    }

    // Previous-pass bound. None means "first journal for this
    // session" — we summarize from the beginning.
    let prev_bound = store::latest_to_entry_id(&ctx.session_id).unwrap_or(None);
    let prev_structured: Option<String> = store::recent_for_session(&ctx.session_id, 1)
        .ok()
        .and_then(|v| v.into_iter().next())
        .map(|r| r.structured_json);
    let previous_journal_id: Option<i64> = store::recent_for_session(&ctx.session_id, 1)
        .ok()
        .and_then(|v| v.into_iter().next())
        .map(|r| r.id);

    let slice = match store::load_slice_after(&ctx.session_id, prev_bound.as_deref()) {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(e),
    };
    if slice.is_empty() {
        return Outcome::SkippedEmptySlice;
    }

    // Extract parallel vectors: ids for the bookkeeping row,
    // messages for the pipeline. Vec<(id, msg)> is awkward to
    // feed into format_prompt which wants &[ChatMessage], so
    // split here.
    let from_entry_id = slice.first().map(|(id, _)| id.clone()).unwrap_or_default();
    let to_entry_id = slice.last().map(|(id, _)| id.clone()).unwrap_or_default();
    let mut messages: Vec<ChatMessage> = slice.into_iter().map(|(_, m)| m).collect();

    // Pre-filter BEFORE redaction. Redaction mutates text; the
    // filter should see the raw signal. Redaction only removes
    // high-entropy secrets, which aren't signal words anyway, so
    // the outcome would be the same either way — but running
    // filter first is cheaper when we're going to skip.
    match prefilter::classify(&messages) {
        Verdict::Skip { reason } => return Outcome::SkippedLowSignal { reason },
        Verdict::Proceed { .. } => {}
    }

    // Redact credentials in place. prompt::format_prompt reads
    // the (now cleaned) slice and returns the final prompt string.
    redact::redact_history_in_place(&mut messages);

    let now_iso = format_iso_now();
    let user_prompt = prompt::format_prompt(&messages, prev_structured.as_deref(), &now_iso);
    let tokens_in_est = estimate_tokens(&user_prompt);

    // LLM call. Resolve the model override if the user set one
    // via env; otherwise use the session's active model.
    let summary_model = std::env::var("SIDEKAR_JOURNAL_MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ctx.model.clone());

    let raw_response = match call_summarizer(&ctx.provider, &summary_model, &user_prompt).await {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(e),
    };
    let tokens_out_est = estimate_tokens(&raw_response);

    // Parse. Hard failures produce a degraded row; we still
    // persist it because (a) the `was_degraded` flag is visible
    // to log consumers, and (b) dropping the row entirely would
    // leave the from/to_entry_id bound unrecorded, so the NEXT
    // pass would re-summarize the same turns at double cost.
    let outcome = parse::parse_response(&raw_response);
    let (journal, degraded, parse_reason) = (outcome.journal, outcome.was_degraded, outcome.reason);

    // Threat scan every field. Even the LLM's own output can carry
    // a prompt-injection it inherited from a poisoned turn in the
    // transcript — we scrub before storing.
    let (cleaned, threat_labels) = scan::scan_journal(&journal);

    let headline = parse::extract_headline(&cleaned);
    let structured_json = match serde_json::to_string(&cleaned) {
        Ok(s) => s,
        Err(e) => {
            return Outcome::Failed(anyhow!("serialize cleaned journal: {e}"));
        }
    };

    let insert = JournalInsert {
        session_id: &ctx.session_id,
        project: &ctx.project,
        from_entry_id: &from_entry_id,
        to_entry_id: &to_entry_id,
        structured_json: &structured_json,
        headline: &headline,
        previous_id: previous_journal_id,
        model_used: &summary_model,
        cred_used: &ctx.cred_name,
        tokens_in: tokens_in_est,
        tokens_out: tokens_out_est,
        created_at: None,
    };

    let inserted_id = match store::insert_journal(&insert) {
        Ok(id) => id,
        Err(e) => return Outcome::Failed(e),
    };

    // Run the memory promoter. Promotion is idempotent — repeat
    // calls reinforce existing memories via the dedup path rather
    // than duplicating rows. Failures here are non-fatal: the
    // journal row is already persisted, so at worst we miss an
    // opportunity to promote this pass and pick it up next time.
    match promote::run_for_project(&ctx.project) {
        Ok(outcome) if outcome.constraints_promoted + outcome.decisions_promoted > 0 => {
            broker::try_log_event(
                "info",
                "journal",
                "promoted",
                Some(&format!(
                    "project={} constraints={} decisions={} memory_ids={:?}",
                    ctx.project,
                    outcome.constraints_promoted,
                    outcome.decisions_promoted,
                    outcome.new_memory_ids,
                )),
            );
        }
        Ok(_) => {
            // Scan ran, nothing reached the threshold. Normal.
        }
        Err(e) => {
            broker::try_log_error("journal", &format!("promote failed: {e:#}"), None);
        }
    }

    if degraded {
        // Not an error-level event — parser degradation is expected
        // occasionally and the row is still useful. Log at info
        // level so operators can spot trends.
        broker::try_log_event(
            "info",
            "journal",
            "degraded-parse",
            Some(&format!(
                "session={} id={} reason={}",
                ctx.session_id, inserted_id, parse_reason
            )),
        );
    }
    if !threat_labels.is_empty() {
        broker::try_log_event(
            "warn",
            "journal",
            "threat-match",
            Some(&format!(
                "session={} id={} labels={}",
                ctx.session_id,
                inserted_id,
                threat_labels.join(","),
            )),
        );
    }

    Outcome::Persisted {
        id: inserted_id,
        tokens_in: tokens_in_est,
        tokens_out: tokens_out_est,
        degraded,
        threat_labels,
    }
}

/// Spawn the polling loop. Lives for the duration of the REPL
/// session; completes when the caller drops the returned
/// `tokio::task::JoinHandle` (typically: on `/new`, `/session`,
/// or REPL exit, by aborting the handle).
///
/// The loop is a trivial `sleep + should_fire + run_once` cycle.
/// Separated from `run_once` so `/journal now` (step 10) can
/// invoke the orchestrator synchronously without the poll timing.
pub(crate) fn spawn_polling_loop(
    ctx: Context,
    tracker: Arc<IdleTracker>,
) -> tokio::task::JoinHandle<()> {
    let threshold = Duration::from_secs(
        std::env::var("SIDEKAR_JOURNAL_IDLE_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_IDLE_SECS),
    );

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            // Fast exits. Checking runtime::journal() here too
            // means a mid-session `/journal off` stops future
            // firings without waiting for the call inside run_once
            // to reject (saves the DB lookups on the pre-check).
            if !runtime::journal() {
                continue;
            }
            if !tracker.should_fire(threshold) {
                continue;
            }

            // Mark fired BEFORE run_once so that a slow LLM call
            // doesn't allow a second pass to queue behind it. If
            // run_once returns an error, the next Done event will
            // re-arm the tracker and fire a fresh attempt then.
            tracker.record_fired();

            match run_once(&ctx).await {
                Outcome::Persisted { id, .. } => {
                    if runtime::verbose() {
                        eprintln!("\x1b[2m[journal #{id} written]\x1b[0m");
                    }
                }
                Outcome::Failed(e) => {
                    broker::try_log_error("journal", &format!("pass failed: {e:#}"), None);
                }
                _skipped => {
                    // Skips are expected — over-budget, empty
                    // slice, low signal. Don't spam logs.
                }
            }
        }
    })
}

/// LLM call: stream a non-tool-using completion, drain the text
/// deltas into a string. Mirrors the pattern used by
/// `src/agent/compaction.rs::summarize_with_llm`, simplified
/// because we don't need a callback for user-visible status.
async fn call_summarizer(
    provider: &Arc<Provider>,
    model: &str,
    user_prompt: &str,
) -> Result<String> {
    let messages = vec![ChatMessage {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: user_prompt.to_string(),
        }],
    }];

    let (mut rx, _reclaim) = provider
        .stream(
            model,
            // System prompt: crisp role instruction. The real
            // content instructions live in the user message (the
            // prompt_header.txt + schema + transcript).
            "You are a precise session summarizer. Follow the \
             user-message instructions exactly and output only \
             valid JSON matching the schema. No commentary.",
            &messages,
            &[],
            // Prompt cache key: per-session, lets providers like
            // Anthropic reuse the system prompt prefix across
            // iterative passes.
            Some("sidekar-journal"),
            None,
            None,
        )
        .await?;

    let mut text = String::new();
    let mut last_error: Option<String> = None;
    while let Some(event) = rx.recv().await {
        match event {
            StreamEvent::TextDelta { delta } => text.push_str(&delta),
            StreamEvent::Error { message } => last_error = Some(message),
            StreamEvent::Done { .. } => break,
            _ => {}
        }
    }

    if let Some(err) = last_error {
        return Err(anyhow!("summarizer stream error: {err}"));
    }
    if text.is_empty() {
        return Err(anyhow!("summarizer returned empty response"));
    }
    Ok(text)
}

fn estimate_tokens(s: &str) -> i64 {
    // Heuristic: 4 chars per token. Cheap, not precise. The cap
    // check uses this, so a systematic over/undercount just
    // shifts the cap uniformly — still a functioning guardrail.
    (s.len() as i64 + 3) / 4
}

fn now_unix_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn format_iso_now() -> String {
    // Minimal ISO-8601-ish: `YYYY-MM-DDTHH:MM:SSZ`. We avoid the
    // chrono dep chain for a journal-header timestamp.
    let secs = now_unix_secs() as i64;
    let (y, m, d, hh, mm, ss) = epoch_to_ymdhms(secs);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Minimal Gregorian-calendar conversion. Correct for the years
/// we care about (1970-9999). Extracted so the rest of the file
/// can stay focused on pipeline logic. Used only for the prompt's
/// `now_iso` placeholder — a display concern, not a correctness-
/// critical one.
fn epoch_to_ymdhms(mut secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    if secs < 0 {
        secs = 0;
    }
    let ss = (secs % 60) as u32;
    let total_minutes = secs / 60;
    let mm = (total_minutes % 60) as u32;
    let total_hours = total_minutes / 60;
    let hh = (total_hours % 24) as u32;
    let mut days_since_epoch = total_hours / 24;

    let mut year: i32 = 1970;
    loop {
        let leap = is_leap(year);
        let days_in_year = if leap { 366 } else { 365 };
        if days_since_epoch >= days_in_year {
            days_since_epoch -= days_in_year;
            year += 1;
        } else {
            break;
        }
    }

    let months: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month: u32 = 1;
    let mut remaining = days_since_epoch;
    for (i, &days_in_month) in months.iter().enumerate() {
        let mut dim = days_in_month;
        if i == 1 && is_leap(year) {
            dim = 29;
        }
        if remaining >= dim {
            remaining -= dim;
            month += 1;
        } else {
            break;
        }
    }
    let day = (remaining + 1) as u32;
    (year, month, day, hh, mm, ss)
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_to_ymdhms_known_anchors() {
        // Unix epoch.
        assert_eq!(epoch_to_ymdhms(0), (1970, 1, 1, 0, 0, 0));
        // 2000-03-01 00:00:00 — 30 * 365.2425 * 86400 style but
        // we'll use a fixed known value. 951868800 = 2000-03-01
        // 00:00:00 UTC.
        assert_eq!(epoch_to_ymdhms(951_868_800), (2000, 3, 1, 0, 0, 0));
        // 2024-02-29 12:34:56 — leap day edge.
        assert_eq!(epoch_to_ymdhms(1_709_210_096), (2024, 2, 29, 12, 34, 56));
        // Midnight at the start of 2026. Authoritative: POSIX
        // timestamp 1_767_225_600 == 2026-01-01T00:00:00Z.
        assert_eq!(epoch_to_ymdhms(1_767_225_600), (2026, 1, 1, 0, 0, 0));
    }

    #[test]
    fn estimate_tokens_is_char_quartering() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abc"), 1); // 3 chars -> ceil(3/4) == 1
        assert_eq!(estimate_tokens("abcd"), 1); // exact quarter
        assert_eq!(estimate_tokens("abcde"), 2); // 5 chars -> 2
        assert_eq!(estimate_tokens(&"x".repeat(400)), 100);
    }

    #[test]
    fn format_iso_now_shape() {
        let s = format_iso_now();
        assert_eq!(s.len(), 20, "expected `YYYY-MM-DDTHH:MM:SSZ`, got {s:?}");
        assert!(s.ends_with('Z'));
        assert!(s.chars().nth(4) == Some('-'));
        assert!(s.chars().nth(10) == Some('T'));
    }
}
