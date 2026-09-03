//! The marker lists question detection matches against, loaded from the store.
//!
//! These started as `const` arrays. The problem with that is release cadence:
//! agent TUIs reword their prompts and swap their glyphs on their own schedule,
//! and a hardcoded list makes every one of those changes a sidekar release
//! before anyone's detection works again. Worse, the failure is silent — a
//! marker that no longer matches just means questions stop being noticed, and
//! bus messages start getting typed into permission dialogs as the answer.
//!
//! So the lists ship as defaults and live in the same SQLite-backed store as
//! the model prompts (see [`crate::prompts`]), which already solves the hard
//! parts: seeding on first use, refreshing untouched entries on upgrade, and
//! protecting anything the user edited. Adding a marker is now an edit, not a
//! release.
//!
//! Reads are cached for the life of the process. Detection runs on every chunk
//! of PTY output, which is far too hot for a database round trip.

use std::sync::OnceLock;

/// Marker lists in the form the matcher wants them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rules {
    /// Phrases meaning the agent is blocked on a human answer.
    pub question_markers: Vec<String>,
    /// Inline yes/no affordances.
    pub yes_no_markers: Vec<String>,
    /// Glyphs that mark the user's input line.
    pub composer_markers: Vec<char>,
}

impl Rules {
    /// Parse the three stored documents into matchable lists.
    fn from_documents(question: &str, yes_no: &str, composer: &str) -> Self {
        Self {
            question_markers: parse_lines(question).map(|l| l.to_lowercase()).collect(),
            yes_no_markers: parse_lines(yes_no).map(|l| l.to_lowercase()).collect(),
            // Only the first character is meaningful; the rest of the line is
            // free to carry a note about which agent draws that glyph.
            composer_markers: parse_lines(composer)
                .filter_map(|l| l.chars().next())
                .collect(),
        }
    }

    /// The compiled defaults, ignoring anything stored.
    pub(crate) fn builtin() -> Self {
        use crate::prompts::{
            KEY_DETECT_COMPOSER_MARKERS, KEY_DETECT_QUESTION_MARKERS, KEY_DETECT_YES_NO_MARKERS,
            default_for,
        };
        Self::from_documents(
            default_for(KEY_DETECT_QUESTION_MARKERS),
            default_for(KEY_DETECT_YES_NO_MARKERS),
            default_for(KEY_DETECT_COMPOSER_MARKERS),
        )
    }

    /// The active rules: stored values where present, compiled defaults
    /// otherwise. Resolved once per process.
    pub(crate) fn active() -> &'static Rules {
        static ACTIVE: OnceLock<Rules> = OnceLock::new();
        ACTIVE.get_or_init(|| {
            use crate::prompts::{
                KEY_DETECT_COMPOSER_MARKERS, KEY_DETECT_QUESTION_MARKERS,
                KEY_DETECT_YES_NO_MARKERS, get,
            };
            let loaded = Rules::from_documents(
                &get(KEY_DETECT_QUESTION_MARKERS),
                &get(KEY_DETECT_YES_NO_MARKERS),
                &get(KEY_DETECT_COMPOSER_MARKERS),
            );
            // An edit that empties a list would silently disable that half of
            // detection. Fall back per-list rather than trusting the blank.
            let builtin = Rules::builtin();
            Rules {
                question_markers: or_builtin(loaded.question_markers, builtin.question_markers),
                yes_no_markers: or_builtin(loaded.yes_no_markers, builtin.yes_no_markers),
                composer_markers: or_builtin(loaded.composer_markers, builtin.composer_markers),
            }
        })
    }
}

fn or_builtin<T>(loaded: Vec<T>, builtin: Vec<T>) -> Vec<T> {
    if loaded.is_empty() { builtin } else { loaded }
}

/// Content lines of a marker document: `#` comments and blanks dropped.
fn parse_lines(doc: &str) -> impl Iterator<Item = &str> {
    doc.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
}

#[cfg(test)]
mod tests;
