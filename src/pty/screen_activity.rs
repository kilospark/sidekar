//! Tell an agent that is working from one that is only repainting.
//!
//! "Working" used to mean "wrote any byte in the last three seconds". That held
//! until Claude Code started redrawing its whole screen about five times a
//! second while it sat at a prompt — erase, home, redraw, to animate a shimmer
//! on its status line. A minute of that is ~450KB of output from an agent that
//! is doing nothing, and it never goes three seconds without a byte, so every
//! idle Claude read as busy forever. The poller correctly refused to interrupt a
//! busy agent, so every bus message waited for the 300s force-inject.
//!
//! The bytes change; what they draw does not. So this keeps a model of the
//! rendered screen and asks the question that matters — did the *visible text*
//! change? — at the one moment it can be answered: between frames.
//!
//! Asking mid-frame is useless. A frame that lands across two reads leaves the
//! screen half-drawn at the boundary ("Claude Te" before "Claude Team"), and a
//! half-drawn screen always differs from a whole one. Measured on a real idle
//! capture, comparing at arbitrary read boundaries saw a change on every read;
//! comparing whole frames saw none at all across 171 consecutive idle frames.
//! Frames arrive as short bursts separated by long silences, so "output has been
//! quiet for [`SETTLE_MS`]" is a reliable proxy for "between frames" that needs
//! no knowledge of any particular agent's drawing style.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// How long output must be quiet before the screen counts as between frames.
///
/// Claude Code's idle frames are ~5ms bursts about 200ms apart; 50ms sits well
/// inside that gap while still being short next to [`crate::activity::PTY_OUTPUT_BUSY_MS`].
pub(crate) const SETTLE_MS: u64 = 50;

/// Output that never pauses long enough to settle counts as work after this.
///
/// An agent streaming a long answer can write faster than [`SETTLE_MS`] apart
/// for seconds. It would never settle, so it would never register a change, so
/// without this it would read as idle while flat out — and idle is exactly when
/// the poller pastes into it. A repainting idle agent is never unsettled for
/// longer than one frame, so this cannot fire on the shimmer.
pub(crate) const UNSETTLED_BUSY_MS: u64 = 500;

/// A model of the agent's screen, compared at settle points.
pub(crate) struct ScreenActivity {
    parser: vt100::Parser,
    /// Visible text at the last settle point, or `None` before the first.
    settled: Option<u64>,
    /// When the first byte since the last settle arrived; 0 when settled.
    dirty_since_ms: u64,
}

impl ScreenActivity {
    pub(crate) fn new(cols: u16, rows: u16) -> Self {
        Self {
            // No scrollback: only what is on screen now can say the agent is
            // doing something now.
            parser: vt100::Parser::new(rows, cols, 0),
            settled: None,
            dirty_since_ms: 0,
        }
    }

    /// Take in a chunk of output. Cheap: a state machine step per byte.
    pub(crate) fn feed(&mut self, bytes: &[u8], now_ms: u64) {
        if bytes.is_empty() {
            return;
        }
        self.parser.process(bytes);
        if self.dirty_since_ms == 0 {
            self.dirty_since_ms = now_ms;
        }
    }

    /// Output has been quiet: compare what is drawn now with the last settle.
    ///
    /// Returns true when the visible text changed. The first settle always
    /// counts as a change — an agent that has just drawn its first screen has
    /// done something.
    pub(crate) fn settle(&mut self) -> bool {
        if self.dirty_since_ms == 0 {
            return false;
        }
        self.dirty_since_ms = 0;
        let now = text_hash(&self.parser.screen().contents());
        let changed = self.settled != Some(now);
        self.settled = Some(now);
        changed
    }

    /// How long output has been arriving without a pause to settle in.
    pub(crate) fn unsettled_for_ms(&self, now_ms: u64) -> u64 {
        if self.dirty_since_ms == 0 {
            0
        } else {
            now_ms.saturating_sub(self.dirty_since_ms)
        }
    }

    /// Match the PTY's real size, so text wraps the way the agent drew it.
    ///
    /// Read from the PTY at each settle rather than pushed from the places that
    /// resize it: there are three of those, and a copy that misses one would
    /// model rows the agent is not drawing.
    pub(crate) fn resize(&mut self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        let (cur_rows, cur_cols) = self.parser.screen().size();
        if (cur_cols, cur_rows) != (cols, rows) {
            self.parser.set_size(rows, cols);
        }
    }
}

fn text_hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests;
