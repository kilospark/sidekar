//! Idle tracker for the journaling trigger.
//!
//! Records when the REPL last became "idle" (finished streaming an
//! assistant turn with no follow-up input) and when the user last
//! interacted (any keystroke or pasted input). The background
//! journaling task — landing in step 7 — polls `since_idle()` and
//! fires an LLM summarization when the gap exceeds a configured
//! threshold (default: `SIDEKAR_JOURNAL_IDLE_SECS`, 90s).
//!
//! This module is *only* state. It does not own the timer loop,
//! does not hold a tokio handle, does not read the config. Step 7
//! wires the polling loop and the actual summarizer call. Keeping
//! them separate means the tracker is trivially testable (clock is
//! injectable via the `now` parameter) and the unit under test is
//! just the state machine.
//!
//! Design notes:
//!   - One Mutex<Inner> shared by the per-turn event callback and
//!     the input reader. The hot path (TextDelta ~50-80/s) does
//!     NOT touch this — we only mutate on Done / Error / Input,
//!     which fire at most once per second even during heavy work.
//!     The mutex is therefore uncontended in practice.
//!   - Clock injected via `Instant` parameter so tests don't sleep.
//!   - `session_journal_at` guards against double-firing: once a
//!     journal has been dispatched for a given idle window, the
//!     tracker records when that happened; step 7's polling loop
//!     checks `should_fire()` which only returns true if a fresh
//!     idle window (i.e. a new `arm()` call after `record_fired()`)
//!     has elapsed.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Tracker state. Everything behind a single mutex — see module doc
/// on contention.
#[derive(Debug)]
struct Inner {
    /// Last time `arm()` was called. None means "not currently
    /// idle" — we're either mid-turn or the user is actively typing.
    armed_at: Option<Instant>,
    /// Last time `record_fired()` was called. Used to suppress
    /// re-firing until the tracker has been disarmed and re-armed.
    fired_at: Option<Instant>,
}

impl Inner {
    fn new() -> Self {
        Self {
            armed_at: None,
            fired_at: None,
        }
    }
}

/// Thread-safe tracker. Cheap to clone via Arc. Typical usage:
///
/// ```ignore
/// let tracker = Arc::new(IdleTracker::new());
/// // in on_event callback:
/// if let StreamEvent::Done { .. } = event { tracker.arm(); }
/// // in input reader, before waiting for stdin:
/// tracker.disarm();
/// // in background polling task:
/// if tracker.should_fire(Duration::from_secs(90)) { run_journal().await; }
/// ```
#[derive(Debug)]
pub(crate) struct IdleTracker {
    inner: Mutex<Inner>,
}

impl IdleTracker {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::new()),
        }
    }

    /// Signal that the REPL just became idle — an assistant turn
    /// finished and we're waiting for input. Idempotent: calling
    /// twice without a `disarm()` between keeps the original
    /// arm time (we don't reset the countdown on repeated Done
    /// events, which would be wrong if a tool-loop re-entered
    /// Waiting and then Done without real idleness in between).
    ///
    /// Resets `fired_at` so a new idle window can produce a new
    /// journal.
    pub(crate) fn arm(&self) {
        self.arm_at(Instant::now());
    }

    /// Test hook: arm at a specific instant. Public inside crate
    /// only for the unit tests in this module; consumers use
    /// `arm()`.
    pub(crate) fn arm_at(&self, now: Instant) {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if guard.armed_at.is_none() {
            guard.armed_at = Some(now);
        }
        guard.fired_at = None;
    }

    /// Signal that the user did something (keystroke, bus message,
    /// slash command, new turn starting) — clear the idle state.
    /// Safe to call even when not armed; no-op in that case.
    pub(crate) fn disarm(&self) {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.armed_at = None;
        // Deliberately leave fired_at alone — if we fired during
        // this window and then a keystroke came in, we don't want
        // to re-fire on the *next* idle until we've actually been
        // idle for the full threshold again.
    }

    /// Record that the journaling task has been dispatched for the
    /// current idle window. Subsequent `should_fire()` calls return
    /// false until `arm()` is called fresh (which requires a
    /// `disarm()` + new Done event in between, i.e. a new turn
    /// completed).
    pub(crate) fn record_fired(&self) {
        self.record_fired_at(Instant::now());
    }

    pub(crate) fn record_fired_at(&self, now: Instant) {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.fired_at = Some(now);
    }

    /// Query: should the background task fire a journal pass now?
    /// True iff:
    ///   - the tracker is currently armed (last event was Done),
    ///   - elapsed since arm >= threshold,
    ///   - we haven't already fired in this idle window.
    pub(crate) fn should_fire(&self, threshold: Duration) -> bool {
        self.should_fire_at(threshold, Instant::now())
    }

    pub(crate) fn should_fire_at(&self, threshold: Duration, now: Instant) -> bool {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let Some(armed_at) = guard.armed_at else {
            return false;
        };
        if guard.fired_at.is_some() {
            return false;
        }
        now.saturating_duration_since(armed_at) >= threshold
    }

    /// Duration since `arm()` was last called, or None if not
    /// currently armed. Exposed for observability (status line,
    /// debug logging) — the firing decision lives in
    /// `should_fire()`.
    #[allow(dead_code)]
    pub(crate) fn since_armed(&self) -> Option<Duration> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.armed_at.map(|t| t.elapsed())
    }
}

impl Default for IdleTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tracker_is_disarmed() {
        let t = IdleTracker::new();
        assert_eq!(t.since_armed(), None);
        assert!(!t.should_fire(Duration::from_secs(1)));
    }

    #[test]
    fn arm_then_disarm_returns_to_idle() {
        let t = IdleTracker::new();
        t.arm();
        assert!(t.since_armed().is_some());
        t.disarm();
        assert_eq!(t.since_armed(), None);
    }

    #[test]
    fn fire_requires_threshold_elapsed() {
        let t = IdleTracker::new();
        let t0 = Instant::now();
        t.arm_at(t0);
        // 50ms after arm with 100ms threshold => not yet.
        assert!(!t.should_fire_at(Duration::from_millis(100), t0 + Duration::from_millis(50)));
        // 150ms after arm with 100ms threshold => fire.
        assert!(t.should_fire_at(Duration::from_millis(100), t0 + Duration::from_millis(150)));
    }

    #[test]
    fn record_fired_suppresses_second_trigger() {
        let t = IdleTracker::new();
        let t0 = Instant::now();
        t.arm_at(t0);
        assert!(t.should_fire_at(Duration::from_millis(50), t0 + Duration::from_millis(100)));
        // Dispatch happens; tracker records it.
        t.record_fired_at(t0 + Duration::from_millis(110));
        // Polling loop tries again at t0+200 — must NOT re-fire
        // even though threshold is still exceeded.
        assert!(!t.should_fire_at(Duration::from_millis(50), t0 + Duration::from_millis(200)));
    }

    #[test]
    fn disarm_then_rearm_allows_new_fire() {
        let t = IdleTracker::new();
        let t0 = Instant::now();
        t.arm_at(t0);
        t.record_fired_at(t0 + Duration::from_millis(10));
        // User typed — disarm. Then next turn finished — re-arm.
        t.disarm();
        t.arm_at(t0 + Duration::from_millis(100));
        // After 100ms from the new arm with 50ms threshold => fire.
        assert!(
            t.should_fire_at(Duration::from_millis(50), t0 + Duration::from_millis(200)),
            "re-armed tracker should fire again after threshold"
        );
    }

    #[test]
    fn double_arm_keeps_original_arm_time() {
        // Rationale: tool loops can produce sequential Done events
        // (one per tool-call iteration on some providers). Resetting
        // the clock on every Done would defeat the point — we want
        // 'threshold since we last TALKED', not 'since the loop
        // last yielded'.
        let t = IdleTracker::new();
        let t0 = Instant::now();
        t.arm_at(t0);
        t.arm_at(t0 + Duration::from_millis(50));
        // 100ms after the first arm with 90ms threshold => fire.
        assert!(
            t.should_fire_at(Duration::from_millis(90), t0 + Duration::from_millis(100)),
            "second arm at +50ms should not have reset the clock"
        );
    }

    #[test]
    fn disarm_clears_fired_flag_for_next_window() {
        // Invariant we care about: a user keystroke (disarm)
        // followed by a fresh Done (re-arm) must allow a new
        // journal to fire. See disarm_then_rearm_allows_new_fire
        // — this test covers the edge where we fired BEFORE
        // disarm and then rearm immediately.
        let t = IdleTracker::new();
        let t0 = Instant::now();
        t.arm_at(t0);
        t.record_fired_at(t0 + Duration::from_millis(10));
        t.disarm();
        // A Done fires microseconds later.
        t.arm_at(t0 + Duration::from_millis(11));
        // Still within threshold after the new arm — no fire yet.
        assert!(!t.should_fire_at(
            Duration::from_millis(50),
            t0 + Duration::from_millis(20)
        ));
        // Past threshold from the new arm => fire.
        assert!(t.should_fire_at(Duration::from_millis(50), t0 + Duration::from_millis(100)));
    }

    #[test]
    fn disarm_from_unarmed_is_noop() {
        let t = IdleTracker::new();
        t.disarm(); // should not panic
        assert_eq!(t.since_armed(), None);
    }

    #[test]
    fn poisoned_mutex_recovered() {
        use std::sync::Arc;
        use std::thread;

        let t = Arc::new(IdleTracker::new());
        let t_clone = t.clone();
        let _ = thread::spawn(move || {
            // Acquire + panic to poison.
            let _g = t_clone.inner.lock();
            panic!("intentional test poison");
        })
        .join();

        // Subsequent calls must still succeed (into_inner path).
        t.arm();
        assert!(t.since_armed().is_some());
    }
}
