//! Read agent activity out of OSC sequences instead of off the screen.
//!
//! Screen scraping (see [`super::waiting`]) infers state from the shape of an
//! agent's TUI, which changes release to release. OSC sequences are different:
//! the agent *declares* them for the terminal to consume. A spinner glyph in
//! the window title while a turn runs, or an OSC 9;4 progress report, is a
//! statement of intent rather than a guess about pixels, so it survives TUI
//! redesigns that break every regex written against the grid.
//!
//! Sidekar already sits on this byte stream — [`super::escape_filter`] rewrites
//! OSC 0/2 payloads to prefix the agent nickname before they reach the user's
//! terminal. This module reads the same payloads on the way past instead of
//! discarding what they say.
//!
//! Signals here are advisory: they feed the same spinner clock that line-based
//! status detection feeds, so a wrong guess decays on the usual timer rather
//! than pinning the agent into a state.

/// What an OSC sequence said about the agent's turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OscSignal {
    /// A turn is in flight (animated title glyph, or active progress report).
    Working,
    /// The agent explicitly withdrew its progress report.
    Idle,
}

/// True for glyphs that only appear as animation frames in a status indicator.
///
/// These are the families CLIs actually cycle through: Braille patterns, the
/// quadrant and half-circle sets, and the two hourglasses. None of them occur
/// in ordinary title text like a path or a branch name, so a leading one is a
/// reliable "busy" marker.
pub(crate) fn is_progress_glyph(c: char) -> bool {
    matches!(c,
        '\u{2800}'..='\u{28FF}'   // Braille patterns: ⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏
        | '\u{25D0}'..='\u{25D3}' // half circles: ◐◑◒◓
        | '\u{25F0}'..='\u{25F3}' // quadrant squares: ◰◱◲◳
        | '\u{2596}'..='\u{259F}' // block element bar frames
        | '\u{23F3}'             // ⏳ hourglass flowing (sidekar's own REPL title)
        | '\u{231B}'             // ⌛ hourglass done
    )
}

/// True for glyphs an agent parks in its title once a turn has finished.
fn is_settled_glyph(c: char) -> bool {
    matches!(c, '\u{2705}' | '\u{2714}' | '\u{2713}') // ✅ ✔ ✓
}

/// Longest OSC payload worth buffering across chunk boundaries.
///
/// Titles are short. A sequence that runs past this is not one we classify, so
/// the partial buffer is dropped rather than grown without bound.
const MAX_PAYLOAD: usize = 512;

/// Incremental scanner for OSC 0/1/2 (window title) and OSC 9;4 (progress).
///
/// PTY reads split wherever the kernel decides, so a sequence can straddle two
/// chunks. State is carried between [`feed`](Self::feed) calls to avoid missing
/// a title that lands on a boundary.
#[derive(Debug, Default)]
pub(crate) struct OscStateDetector {
    /// Bytes of an OSC sequence seen so far, without the opening `ESC ]`.
    partial: Vec<u8>,
    /// Inside an OSC sequence, waiting for BEL or ST.
    in_osc: bool,
    /// Trailing lone `ESC` from the previous chunk.
    pending_esc: bool,
}

impl OscStateDetector {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Scan a chunk of agent output, returning the last signal it contained.
    ///
    /// The last one wins: within a single chunk an agent may repaint its title
    /// several times, and only the final value describes the current state.
    pub(crate) fn feed(&mut self, raw: &[u8]) -> Option<OscSignal> {
        let mut signal = None;
        let mut i = 0;

        while i < raw.len() {
            if self.in_osc {
                match osc_terminator(raw, i) {
                    Some((end, skip)) => {
                        self.partial.extend_from_slice(&raw[i..end]);
                        if let Some(sig) = classify_payload(&self.partial) {
                            signal = Some(sig);
                        }
                        self.reset();
                        i = end + skip;
                    }
                    None => {
                        self.partial.extend_from_slice(&raw[i..]);
                        if self.partial.len() > MAX_PAYLOAD {
                            // Not a title or a progress report; stop tracking it.
                            self.reset();
                        }
                        break;
                    }
                }
                continue;
            }

            // `ESC ]` opens an OSC sequence, possibly split across chunks.
            if self.pending_esc {
                self.pending_esc = false;
                if raw[i] == b']' {
                    self.in_osc = true;
                    i += 1;
                    continue;
                }
            }
            if raw[i] == 0x1b {
                if i + 1 < raw.len() {
                    if raw[i + 1] == b']' {
                        self.in_osc = true;
                        i += 2;
                        continue;
                    }
                } else {
                    self.pending_esc = true;
                }
            }
            i += 1;
        }

        signal
    }

    fn reset(&mut self) {
        self.partial.clear();
        self.in_osc = false;
    }
}

/// Offset of the OSC terminator at or after `from`, plus its length.
///
/// Terminators are BEL (1 byte) or ST — `ESC \` (2 bytes).
fn osc_terminator(raw: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i < raw.len() {
        if raw[i] == 0x07 {
            return Some((i, 1));
        }
        if raw[i] == 0x1b && i + 1 < raw.len() && raw[i + 1] == b'\\' {
            return Some((i, 2));
        }
        i += 1;
    }
    None
}

/// Classify an OSC payload (everything between `ESC ]` and the terminator).
fn classify_payload(payload: &[u8]) -> Option<OscSignal> {
    let text = std::str::from_utf8(payload).ok()?;
    let (ps, rest) = text.split_once(';')?;

    match ps {
        // Window title: icon name, window title, or both.
        "0" | "1" | "2" => classify_title(rest),
        // ConEmu-style progress: `9;4;<state>;<percent>`.
        "9" => classify_progress(rest),
        _ => None,
    }
}

/// A leading animation glyph in the title means a turn is running.
fn classify_title(title: &str) -> Option<OscSignal> {
    let first = title.trim_start().chars().next()?;
    if is_progress_glyph(first) {
        Some(OscSignal::Working)
    } else if is_settled_glyph(first) {
        Some(OscSignal::Idle)
    } else {
        // Ordinary title text says nothing either way; leave the clock alone.
        None
    }
}

/// OSC 9;4 progress. State 0 clears the report; 1 (normal), 2 (error) and
/// 3 (indeterminate) all mean a turn is still in flight. 4 (paused) is left
/// alone: paused work is not running, but it is not idle either.
fn classify_progress(rest: &str) -> Option<OscSignal> {
    let rest = rest.strip_prefix("4;")?;
    let state = rest.split(';').next()?;
    match state {
        "0" => Some(OscSignal::Idle),
        "1" | "2" | "3" => Some(OscSignal::Working),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
