//! Terminal input-mode tracking from the agent's own output stream.
//!
//! TUI agents announce how they want input encoded: bracketed paste
//! (`CSI ? 2004 h`), application cursor keys (`CSI ? 1 h`), the kitty keyboard
//! protocol (`CSI > flags u`), and alternate screen / cursor visibility.
//!
//! Sidekar used to guess the paste encoding from the agent's *name*, which
//! breaks for unknown agents and goes stale whenever an agent toggles modes
//! mid-session (permission prompts commonly disable bracketed paste). Reading
//! the modes off the wire is authoritative, so bus injection can encode the
//! way the agent is actually listening right now.
//!
//! The tracked state also produces a replay `preamble()`: the sequences needed
//! to put a freshly attached viewer into the same modes before replaying
//! buffered output.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Maximum bytes of a partial escape sequence carried between `feed` calls.
const MAX_PARSE_TAIL: usize = 64;

/// Live terminal modes parsed from PTY output. Cheap to read from any thread.
#[derive(Debug, Default)]
pub struct TerminalInputMode {
    alternate_screen: AtomicBool,
    cursor_hidden: AtomicBool,
    bracketed_paste: AtomicBool,
    application_cursor_keys: AtomicBool,
    /// Set once any `?2004h`/`?2004l` has been observed, so callers can tell
    /// "agent said no" apart from "agent has not said anything yet".
    bracketed_paste_observed: AtomicBool,
    kitty_flags: AtomicU32,
    kitty_stack: Mutex<Vec<u32>>,
    parse_tail: Mutex<Vec<u8>>,
}

/// Snapshot of the tracked modes, for tests and for building a preamble.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputModeState {
    pub alternate_screen: bool,
    pub cursor_hidden: bool,
    pub bracketed_paste: bool,
    pub application_cursor_keys: bool,
    pub kitty_flags: u32,
}

impl TerminalInputMode {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn alternate_screen(&self) -> bool {
        self.alternate_screen.load(Ordering::Relaxed)
    }

    pub fn cursor_hidden(&self) -> bool {
        self.cursor_hidden.load(Ordering::Relaxed)
    }

    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste.load(Ordering::Relaxed)
    }

    /// `None` until the agent has announced a bracketed-paste mode either way.
    pub fn bracketed_paste_observed(&self) -> Option<bool> {
        self.bracketed_paste_observed
            .load(Ordering::Relaxed)
            .then(|| self.bracketed_paste())
    }

    pub fn application_cursor_keys(&self) -> bool {
        self.application_cursor_keys.load(Ordering::Relaxed)
    }

    pub fn kitty_flags(&self) -> u32 {
        self.kitty_flags.load(Ordering::Relaxed)
    }

    /// True when the agent can distinguish modified Enter (Shift/Alt+Enter)
    /// from a plain carriage return.
    pub fn supports_modified_enter(&self) -> bool {
        self.kitty_flags() > 0
    }

    pub fn state(&self) -> InputModeState {
        InputModeState {
            alternate_screen: self.alternate_screen(),
            cursor_hidden: self.cursor_hidden(),
            bracketed_paste: self.bracketed_paste(),
            application_cursor_keys: self.application_cursor_keys(),
            kitty_flags: self.kitty_flags(),
        }
    }

    /// Sequences that re-establish the current modes on a fresh viewer.
    ///
    /// Order matters: enter the alternate screen first so the replayed output
    /// lands on the same buffer the agent has been drawing to.
    pub fn preamble(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(48);
        if self.alternate_screen() {
            out.extend_from_slice(b"\x1b[?1049h");
        }
        let flags = self.kitty_flags();
        if flags > 0 {
            out.extend_from_slice(format!("\x1b[={flags};1u").as_bytes());
        }
        if self.application_cursor_keys() {
            out.extend_from_slice(b"\x1b[?1h");
        }
        if self.bracketed_paste() {
            out.extend_from_slice(b"\x1b[?2004h");
        }
        if self.cursor_hidden() {
            out.extend_from_slice(b"\x1b[?25l");
        }
        out
    }

    /// Feed a chunk of PTY output. Partial escape sequences are carried over.
    pub fn feed(&self, bytes: &[u8]) {
        let mut data = Vec::new();
        if let Ok(mut tail) = self.parse_tail.lock()
            && !tail.is_empty()
        {
            data.extend_from_slice(&tail);
            tail.clear();
        }
        data.extend_from_slice(bytes);

        let bytes = data.as_slice();
        let mut next_tail: &[u8] = &[];
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] != 0x1b {
                i += 1;
                continue;
            }
            let esc_start = i;
            if i + 1 >= bytes.len() {
                next_tail = &bytes[esc_start..];
                break;
            }
            if bytes[i + 1] != b'[' {
                i += 2;
                continue;
            }
            i += 2;
            // Optional private / kitty intermediate prefix.
            let prefix = if i < bytes.len() && matches!(bytes[i], b'?' | b'>' | b'=' | b'<') {
                let p = bytes[i];
                i += 1;
                Some(p)
            } else {
                None
            };
            let params_start = i;
            while i < bytes.len() && matches!(bytes[i], b'0'..=b'9' | b';') {
                i += 1;
            }
            if i >= bytes.len() {
                next_tail = &bytes[esc_start..];
                break;
            }
            let final_byte = bytes[i];
            let params = &bytes[params_start..i];
            match (prefix, final_byte) {
                (Some(b'?'), b'h') | (Some(b'?'), b'l') => {
                    self.apply_private_mode(params, final_byte == b'h');
                }
                (_, b'u') => self.apply_kitty_keyboard(prefix, params),
                _ => {}
            }
            i += 1;
        }

        if let Ok(mut tail) = self.parse_tail.lock() {
            tail.clear();
            // A pathological stream of ESC bytes must not grow the tail forever.
            if next_tail.len() <= MAX_PARSE_TAIL {
                tail.extend_from_slice(next_tail);
            }
        }
    }

    fn apply_private_mode(&self, params: &[u8], enabled: bool) {
        for param in params.split(|b| *b == b';') {
            match param {
                b"47" | b"1047" | b"1049" => {
                    self.alternate_screen.store(enabled, Ordering::Relaxed);
                }
                // DECTCEM: `h` shows the cursor, `l` hides it.
                b"25" => self.cursor_hidden.store(!enabled, Ordering::Relaxed),
                b"1" => self
                    .application_cursor_keys
                    .store(enabled, Ordering::Relaxed),
                b"2004" => {
                    self.bracketed_paste.store(enabled, Ordering::Relaxed);
                    self.bracketed_paste_observed.store(true, Ordering::Relaxed);
                }
                _ => {}
            }
        }
    }

    /// Kitty keyboard protocol: `CSI > flags u` push, `CSI < n u` pop,
    /// `CSI = flags ; mode u` set.
    fn apply_kitty_keyboard(&self, prefix: Option<u8>, params: &[u8]) {
        let text = String::from_utf8_lossy(params);
        let mut parts = text.split(';');
        let first = parts.next().unwrap_or("").trim();
        let first_num = first.parse::<u32>().ok();

        match prefix {
            Some(b'>') => {
                let flags = first_num.unwrap_or(0);
                if let Ok(mut stack) = self.kitty_stack.lock() {
                    stack.push(self.kitty_flags());
                    // Bound the stack; a runaway agent must not grow it forever.
                    if stack.len() > 32 {
                        stack.remove(0);
                    }
                }
                self.kitty_flags.store(flags, Ordering::Relaxed);
            }
            Some(b'<') => {
                let count = first_num.unwrap_or(1).max(1);
                if let Ok(mut stack) = self.kitty_stack.lock() {
                    let mut restored = self.kitty_flags();
                    for _ in 0..count {
                        match stack.pop() {
                            Some(v) => restored = v,
                            None => {
                                restored = 0;
                                break;
                            }
                        }
                    }
                    self.kitty_flags.store(restored, Ordering::Relaxed);
                }
            }
            Some(b'=') => {
                let flags = first_num.unwrap_or(0);
                let mode = parts.next().and_then(|m| m.trim().parse::<u32>().ok());
                let current = self.kitty_flags();
                let next = match mode {
                    // 1 = set, 2 = OR in, 3 = AND out. Default is set.
                    Some(2) => current | flags,
                    Some(3) => current & !flags,
                    _ => flags,
                };
                self.kitty_flags.store(next, Ordering::Relaxed);
            }
            // `CSI ? u` is the query response; `CSI u` alone is not a mode change.
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
