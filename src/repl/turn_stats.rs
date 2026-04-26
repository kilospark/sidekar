//! Per-session, per-turn usage accumulator.
//!
//! `/status` needs two things the REPL wasn't tracking:
//!   1. Cumulative token usage across all turns in the current session
//!      (input / output / cache-read / cache-write).
//!   2. The most recent turn's usage in isolation, so users can see
//!      "what did that last message cost me" at a glance.
//!
//! The renderer already prints a dim `[X in / Y out …]` line after
//! each response; that value evaporates immediately. This module
//! keeps a running tally that `/status` reads.
//!
//! Kept in its own file — and in its own mutex — to avoid contending
//! with the renderer's lock. The renderer is on the hot path of every
//! streaming delta (~50-80/s); TurnStats is touched only at
//! StreamEvent::Done (~1/turn). Sharing a mutex would mean every
//! token render would briefly block any reader, including a user
//! typing `/status` during a long stream.
//!
//! Kept *out* of the session DB because it's cheap to reconstruct
//! from the Usage field embedded in each saved assistant message if
//! we ever want historical totals. Persistence is a separate concern.

use crate::providers::{AssistantResponse, StopReason, Usage};

/// Running totals for the current REPL session. All timestamps are
/// `Instant` rather than `SystemTime` so elapsed-time display is
/// monotonic across wall-clock changes; the session creation time
/// shown by `/status` comes from the session DB, not here.
pub(super) struct TurnStats {
    /// Sum of every Usage reported by the provider on StreamEvent::Done
    /// during this session. A zero-turn session has all fields zero.
    pub cumulative: Usage,
    /// Count of Done events observed. Not equal to `history.len() / 2`
    /// because compaction can rewrite history, and tool turns can emit
    /// multiple Done events per user prompt.
    pub turn_count: u32,
    /// Usage reported by the most recent Done event. None before any
    /// turn completes. Kept separately so `/status` can show "last
    /// turn" numbers even after cumulative has absorbed them.
    pub last: Option<Usage>,
    /// Stop reason from the most recent Done event. None before any
    /// turn completes. Useful for surfacing premature-stop diagnoses
    /// (Length / Aborted / Error) in /status without scrolling back.
    pub last_stop_reason: Option<StopReason>,
    /// Provider response id from the most recent Done event. Empty
    /// string for providers that don't emit one. Included in /status
    /// mainly so a user filing a support ticket can copy the id
    /// without digging through logs.
    pub last_response_id: String,
    /// When the last turn completed, for "X min ago" relative display.
    /// None before any turn completes.
    pub last_turn_at: Option<std::time::Instant>,
    /// When this TurnStats was constructed (≈ REPL session start).
    /// Shown in /status as "up <duration>".
    pub session_started_at: std::time::Instant,
}

impl TurnStats {
    pub(super) fn new() -> Self {
        Self {
            cumulative: Usage::default(),
            turn_count: 0,
            last: None,
            last_stop_reason: None,
            last_response_id: String::new(),
            last_turn_at: None,
            session_started_at: std::time::Instant::now(),
        }
    }

    /// Absorb the Usage from a completed turn. Called from the
    /// `StreamEvent::Done` branch of the main event callback. No-op
    /// for non-Done events — caller is responsible for only invoking
    /// this on Done.
    pub(super) fn record(&mut self, msg: &AssistantResponse) {
        let u = &msg.usage;
        self.cumulative.input_tokens = self.cumulative.input_tokens.saturating_add(u.input_tokens);
        self.cumulative.output_tokens = self
            .cumulative
            .output_tokens
            .saturating_add(u.output_tokens);
        self.cumulative.cache_read_tokens = self
            .cumulative
            .cache_read_tokens
            .saturating_add(u.cache_read_tokens);
        self.cumulative.cache_write_tokens = self
            .cumulative
            .cache_write_tokens
            .saturating_add(u.cache_write_tokens);
        self.turn_count = self.turn_count.saturating_add(1);
        self.last = Some(u.clone());
        self.last_stop_reason = Some(msg.stop_reason.clone());
        self.last_response_id = msg.response_id.clone();
        self.last_turn_at = Some(std::time::Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ContentBlock, StopReason};

    fn resp(u: Usage, stop: StopReason, rid: &str) -> AssistantResponse {
        AssistantResponse {
            content: vec![ContentBlock::Text {
                text: String::new(),
            }],
            usage: u,
            stop_reason: stop,
            model: "m".into(),
            response_id: rid.into(),
            rate_limit: None,
        }
    }

    #[test]
    fn record_accumulates_across_turns() {
        let mut s = TurnStats::new();
        assert_eq!(s.turn_count, 0);
        assert!(s.last.is_none());

        s.record(&resp(
            Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 10,
                cache_write_tokens: 5,
            },
            StopReason::Stop,
            "rid-1",
        ));
        assert_eq!(s.turn_count, 1);
        assert_eq!(s.cumulative.input_tokens, 100);
        assert_eq!(s.cumulative.output_tokens, 50);
        assert_eq!(s.last.as_ref().unwrap().input_tokens, 100);
        assert_eq!(s.last_response_id, "rid-1");

        s.record(&resp(
            Usage {
                input_tokens: 200,
                output_tokens: 30,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
            StopReason::ToolUse,
            "rid-2",
        ));
        assert_eq!(s.turn_count, 2);
        assert_eq!(s.cumulative.input_tokens, 300);
        assert_eq!(s.cumulative.output_tokens, 80);
        assert_eq!(s.cumulative.cache_read_tokens, 10);
        // `last` is the most recent only — not the running sum.
        assert_eq!(s.last.as_ref().unwrap().input_tokens, 200);
        assert_eq!(s.last_response_id, "rid-2");
        assert!(matches!(
            s.last_stop_reason,
            Some(StopReason::ToolUse)
        ));
    }

    #[test]
    fn saturating_math_survives_u32_overflow() {
        // Not a realistic scenario — but a rogue provider could
        // report garbage, and we'd rather cap than panic.
        let mut s = TurnStats::new();
        s.cumulative.input_tokens = u32::MAX - 10;
        s.record(&resp(
            Usage {
                input_tokens: 100,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
            StopReason::Stop,
            "",
        ));
        assert_eq!(s.cumulative.input_tokens, u32::MAX);
    }
}
