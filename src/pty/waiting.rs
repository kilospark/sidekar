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

mod rules;

pub(crate) use rules::Rules;

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
    #[cfg(test)]
    pub(crate) fn is_question_on_screen(&self) -> bool {
        looks_like_question(&self.tail)
    }

    /// The matched question and why it matched, for `bus explain`.
    pub(crate) fn question_on_screen(&self) -> Option<QuestionMatch> {
        match_question(&self.tail, Rules::active())
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

/// Vertical box-drawing glyphs agents draw down the sides of the composer.
const BOX_VERTICALS: &[char] = &['│', '┃', '║', '|'];

/// The parts of the screen a rule can be matched against.
///
/// Matching the whole tail treats the agent's UI and the human's half-typed
/// draft as the same text, which is how a user typing "do you want to rebase?"
/// into the composer reads as the *agent* asking a question. Scoping each rule
/// to a region keeps the two apart.
struct Regions<'a> {
    /// Last [`INSPECT_LINES`] non-empty lines, trailing space trimmed.
    all: Vec<&'a str>,
    /// [`all`](Self::all) minus the composer, i.e. only what the agent drew.
    outside_composer: Vec<&'a str>,
}

impl<'a> Regions<'a> {
    fn new(tail: &'a str, rules: &Rules) -> Self {
        let lines: Vec<&str> = tail
            .lines()
            .map(|line| line.trim_end())
            .filter(|line| !line.trim().is_empty())
            .collect();
        let start = lines.len().saturating_sub(INSPECT_LINES);
        let all = lines[start..].to_vec();
        let outside_composer = all
            .iter()
            .copied()
            .filter(|line| !is_composer_line(line, rules))
            .collect();
        Self {
            all,
            outside_composer,
        }
    }
}

/// Strip a box border from both ends of a line, leaving the content inside.
///
/// Composers are usually drawn inside a rounded box, so the raw line is
/// `│ > half-typed text │` rather than `> half-typed text`.
fn unbox(line: &str) -> &str {
    let trimmed = line.trim();
    let trimmed = trimmed.strip_prefix(BOX_VERTICALS).unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix(BOX_VERTICALS).unwrap_or(trimmed);
    trimmed.trim()
}

/// True when the line is the agent's input line — where the *human* types.
///
/// A selection cursor sitting on a numbered option (`❯ 1. Yes`) uses the same
/// glyph as a composer prompt, so options are excluded explicitly; otherwise
/// scoping a rule away from the composer would also hide every permission
/// dialog, turning a false positive into a much worse false negative.
fn is_composer_line(line: &str, rules: &Rules) -> bool {
    let inner = unbox(line);
    let Some(rest) = inner.strip_prefix(|c: char| rules.composer_markers.contains(&c)) else {
        return false;
    };
    if option_line_cursor(inner).is_some() {
        return false;
    }
    rest.is_empty() || rest.starts_with(' ')
}

/// Why detection concluded the agent is blocked.
///
/// Detection is a heuristic over someone else's UI, so it will be wrong
/// sometimes. Carrying the cause means a wrong answer can be diagnosed from
/// `bus explain` instead of guessed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QuestionMatch {
    /// Which rule fired: a marker phrase, or the option-list shape.
    pub rule: String,
    /// The line it fired on, trimmed for display.
    pub line: String,
}

impl QuestionMatch {
    pub(crate) fn describe(&self) -> String {
        format!("{} matched on {:?}", self.rule, truncate(&self.line, 72))
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Core matcher, split out so it can be tested on plain strings.
#[cfg(test)]
pub(crate) fn looks_like_question(tail: &str) -> bool {
    looks_like_question_with(tail, Rules::active())
}

/// Match against an explicit rule set, so tests need no database.
#[cfg(test)]
pub(crate) fn looks_like_question_with(tail: &str, rules: &Rules) -> bool {
    match_question(tail, rules).is_some()
}

/// The matching rule, if any.
pub(crate) fn match_question(tail: &str, rules: &Rules) -> Option<QuestionMatch> {
    let regions = Regions::new(tail, rules);
    if regions.all.is_empty() {
        return None;
    }

    // A numbered option list under a selection cursor is the dominant shape:
    //   ❯ 1. Yes
    //     2. Yes, and don't ask again
    //     3. No, and tell Claude what to do differently
    if let Some(line) = option_list_line(&regions.outside_composer) {
        return Some(QuestionMatch {
            rule: "option list".to_string(),
            line: line.trim().to_string(),
        });
    }

    // Phrase markers are matched only on what the agent drew. The composer
    // holds the human's own words, which routinely contain these phrases.
    for line in &regions.outside_composer {
        let lowered = line.to_lowercase();
        if let Some(marker) = rules
            .yes_no_markers
            .iter()
            .chain(&rules.question_markers)
            .find(|m| lowered.contains(m.as_str()))
        {
            return Some(QuestionMatch {
                rule: format!("marker {marker:?}"),
                line: line.trim().to_string(),
            });
        }
    }

    // Below this point nothing depends on wording a marker list had to be
    // written for. First-run wizards, license accepts and trust dialogs from
    // *any* wrapped CLI draw one of a small number of generic prompt shapes;
    // catching the shape means a new agent's onboarding never needs its own
    // release before `bus wait` can see it.
    for line in &regions.outside_composer {
        let content = unbox(line);
        if content.is_empty() {
            continue;
        }
        let lowered = content.to_lowercase();
        // "Press any key to continue", "Press 'q' to quit", "Press ESC to skip" —
        // generalizes the fixed "press enter to ..." markers to any key name.
        if lowered.contains("press ") && lowered.contains(" to ") {
            return Some(QuestionMatch {
                rule: "press-to imperative".to_string(),
                line: line.trim().to_string(),
            });
        }
        if ends_with_bracketed_choices(content) {
            return Some(QuestionMatch {
                rule: "bracketed choices".to_string(),
                line: line.trim().to_string(),
            });
        }
    }

    // Last resort: a question the agent is blocked on is always the last
    // thing drawn, so a trailing `?` on the most recent line reads as a live
    // question even in wording no marker anticipated. Scoped to the last
    // line only (not the whole window) to avoid flagging a rhetorical
    // question buried in old scrollback.
    if let Some(line) = regions.outside_composer.last() {
        let content = unbox(line);
        if content.ends_with('?') && content.chars().count() <= 200 {
            return Some(QuestionMatch {
                rule: "trailing question mark".to_string(),
                line: line.trim().to_string(),
            });
        }
    }

    None
}

/// True when `line` ends with a short bracketed set of alternatives, e.g.
/// `(y/n)`, `[Y/n/a]`, `(accept/skip)` — the shape a CLI uses for a
/// single-keystroke prompt, independent of which words it picked. Generalizes
/// the fixed yes/no marker list to wording it was never written for.
fn ends_with_bracketed_choices(line: &str) -> bool {
    let trimmed = line.trim_end_matches(['?', ':', '.', ' ']);
    let (open, close) = if trimmed.ends_with(')') {
        ('(', ')')
    } else if trimmed.ends_with(']') {
        ('[', ']')
    } else {
        return false;
    };
    let Some(start) = trimmed.rfind(open) else {
        return false;
    };
    // A bracket glued onto an alphanumeric word (`build(target/release)`) is a
    // shell-command/path shape, not a prompt's choice list — real prompts set
    // the bracket off with whitespace or punctuation (`Continue? (y/n)`).
    if let Some(prev) = trimmed[..start].chars().last() {
        if prev.is_alphanumeric() {
            return false;
        }
    }
    let inner = &trimmed[start + 1..trimmed.len() - close.len_utf8()];
    if inner.is_empty() || inner.len() > 40 {
        return false;
    }
    let parts: Vec<&str> = inner.split('/').collect();
    (2..=4).contains(&parts.len())
        && parts.iter().all(|p| {
            let p = p.trim();
            !p.is_empty() && p.len() <= 12 && p.chars().all(|c| c.is_alphanumeric())
        })
}

/// The cursor line of an option list, when at least two options are present.
fn option_list_line<'a>(lines: &[&'a str]) -> Option<&'a str> {
    let mut numbered = 0usize;
    let mut cursor_line = None;

    for line in lines {
        if let Some(has_cursor) = option_line_cursor(line) {
            numbered += 1;
            if has_cursor && cursor_line.is_none() {
                cursor_line = Some(*line);
            }
        }
    }

    if numbered >= 2 { cursor_line } else { None }
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
