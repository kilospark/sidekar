//! Detect that the wrapped agent is parked on a question and needs a human.
//!
//! Paseo learns this from hooks it installs into each agent's config file
//! (`UserPromptSubmit` → running, `Notification`/`idle_prompt` → needs input).
//! Sidekar does not own the agents' config files, so it reads the screen
//! instead: agent question UIs are highly stereotyped (numbered option lists,
//! `(y/n)`, "Do you want to ...", "Press Enter to continue").
//!
//! This matters because an idle-looking agent and an agent blocked on a
//! permission prompt are indistinguishable to the byte-activity heuristic: both
//! stop producing output. Injecting a bus message into a question types the
//! message as the *answer*. So a detected question blocks injection until the
//! human resolves it, and surfaces as `ActivityState::NeedsInput`.

/// Plain text kept for pattern matching. Roughly a screenful of an 80x40 term.
const TAIL_CAPACITY: usize = 4096;
/// Only the end of the screen is inspected — a question the agent is blocked on
/// is always the last thing drawn, and prose further up must not match.
const INSPECT_LINES: usize = 12;

/// Phrases that only appear when an agent is blocking on a human answer.
const QUESTION_MARKERS: &[&str] = &[
    "do you want to",
    "do you want me to",
    "would you like me to",
    "press enter to continue",
    "press enter to confirm",
    "waiting for your input",
    "waiting for user input",
    "allow this command",
    "allow command",
    "approve this",
    "apply this change",
    "continue?",
    "proceed?",
    "overwrite?",
    "are you sure",
];

/// Inline yes/no affordances, matched case-insensitively on the tail lines.
const YES_NO_MARKERS: &[&str] = &["(y/n)", "[y/n]", "(yes/no)", "(y/n/a)", "[y/n/a]"];

/// Rolling window of ANSI-stripped agent output used for question detection.
#[derive(Debug, Default)]
pub(crate) struct WaitingDetector {
    tail: String,
}

impl WaitingDetector {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Feed already-ANSI-stripped text from the agent's output.
    pub(crate) fn feed_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.tail.push_str(text);
        if self.tail.len() > TAIL_CAPACITY {
            // Trim on a char boundary; the extra bytes are harmless.
            let cut = self.tail.len() - TAIL_CAPACITY;
            let cut = self
                .tail
                .char_indices()
                .map(|(i, _)| i)
                .find(|i| *i >= cut)
                .unwrap_or(self.tail.len());
            self.tail.drain(..cut);
        }
    }

    /// True when the tail of the screen looks like an unanswered question.
    pub(crate) fn is_question_on_screen(&self) -> bool {
        looks_like_question(&self.tail)
    }

    /// Forget the current screen — called once the question is answered so a
    /// stale tail cannot keep injection blocked.
    pub(crate) fn clear(&mut self) {
        self.tail.clear();
    }
}

/// True when the chunk repaints the whole screen, invalidating the tracked tail.
pub(crate) fn resets_screen(raw: &[u8]) -> bool {
    const RESETS: &[&[u8]] = &[b"\x1b[2J", b"\x1b[3J", b"\x1b[?1049h", b"\x1b[?1049l"];
    RESETS
        .iter()
        .any(|needle| raw.windows(needle.len()).any(|w| w == *needle))
}

/// Core matcher, split out so it can be tested on plain strings.
pub(crate) fn looks_like_question(tail: &str) -> bool {
    let lines: Vec<&str> = tail
        .lines()
        .map(|line| line.trim_end())
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return false;
    }

    let start = lines.len().saturating_sub(INSPECT_LINES);
    let window = &lines[start..];
    let lowered: Vec<String> = window.iter().map(|l| l.to_lowercase()).collect();

    // A numbered option list under a selection cursor is the dominant shape:
    //   ❯ 1. Yes
    //     2. Yes, and don't ask again
    //     3. No, and tell Claude what to do differently
    if has_option_list(window) {
        return true;
    }

    for line in &lowered {
        if YES_NO_MARKERS.iter().any(|m| line.contains(m)) {
            return true;
        }
        if QUESTION_MARKERS.iter().any(|m| line.contains(m)) {
            return true;
        }
    }

    false
}

/// At least two numbered choices, with a selection cursor on one of them.
fn has_option_list(lines: &[&str]) -> bool {
    let mut numbered = 0usize;
    let mut cursor_on_option = false;

    for line in lines {
        if let Some(has_cursor) = option_line_cursor(line) {
            numbered += 1;
            cursor_on_option |= has_cursor;
        }
    }

    numbered >= 2 && cursor_on_option
}

/// `Some(has_cursor)` when the line is `[cursor] <digit>. text`.
///
/// Plain `>` is deliberately not a cursor glyph: a markdown blockquote holding
/// an ordered list ("> 1. ...") would otherwise read as a selection prompt.
fn option_line_cursor(line: &str) -> Option<bool> {
    let trimmed = line.trim_start();
    let (has_cursor, rest) = match trimmed.strip_prefix(['❯', '▶', '➤', '›']) {
        Some(rest) => (true, rest.trim_start()),
        None => (false, trimmed),
    };

    let mut chars = rest.chars();
    let first = chars.next()?;
    if !first.is_ascii_digit() {
        return None;
    }
    let mut rest = chars.as_str();
    // Allow two-digit options.
    if let Some(stripped) = rest.strip_prefix(|c: char| c.is_ascii_digit()) {
        rest = stripped;
    }
    let rest = rest.strip_prefix(['.', ')'])?;
    if !rest.starts_with(' ') {
        return None;
    }
    if rest.trim().is_empty() {
        return None;
    }
    Some(has_cursor)
}

#[cfg(test)]
mod tests;
