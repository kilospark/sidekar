//! Cheap pre-filter to skip journaling passes that would produce
//! nothing worth storing.
//!
//! The background journaling task runs every ~90s of idle time on
//! an active REPL session. If the intervening turns were low-signal
//! ("what time is it?", "thanks"), spending LLM tokens on a journal
//! is pure cost. This module does a fast regex sweep and returns a
//! go/no-go verdict: call the LLM only if something interesting
//! happened.
//!
//! Patterns adapted from the holographic-memory plugin in
//! NousResearch/hermes-agent (`plugins/memory/holographic/__init__.py
//! :358-396`), plus signals specific to coding sessions that
//! hermes's text-focused patterns don't catch:
//!
//!   - User-stated preferences / decisions / constraints
//!     ("I prefer X", "we decided Y", "always Z")
//!   - Tool activity (any ToolCall block = the agent did work)
//!   - File edits (Edit / Write / Create tool names)
//!   - Error / blocker phrasing in assistant output ("failed",
//!     "cannot", "not working", explicit error messages)
//!   - Questions the user asked ("how do I", "why did", "can you")
//!   - Completion signals ("done", "implemented", "fixed")
//!
//! Also: a minimum character budget. Even if none of the signal
//! regexes fire, a long substantive exchange should still be
//! journaled — the model may have decided something implicitly
//! that our regex doesn't catch. Default threshold: 800 chars of
//! non-whitespace content across the slice.
//!
//! Performance: RegexSet-based; one scan of each concatenated text
//! field. Typical cost < 100µs for a 20-turn slice. Effectively
//! free compared to the 2-5s LLM call it gates.

use std::sync::LazyLock;

use regex::RegexSet;

use crate::providers::{ChatMessage, ContentBlock};

/// Minimum non-whitespace character count across the slice that
/// triggers "journal anyway, even if no signal regex fired." Tuned
/// so a genuine multi-turn technical exchange always journals but
/// pure chitchat ("thanks", "ok", "sounds good") doesn't. 800 is
/// roughly 2-3 substantive turns.
const MIN_CHARS_FOR_SUBSTANCE: usize = 800;

/// Signal patterns. Any match anywhere in the slice's text (user
/// messages, assistant text, tool results) is enough to proceed.
/// Case-insensitive, boundary-anchored where appropriate to cut
/// down on ordinary-prose false positives.
static SIGNAL_SET: LazyLock<RegexSet> = LazyLock::new(|| {
    let raw: &[&str] = &[
        // Preferences and standing instructions.
        r"(?i)\bI\s+(?:prefer|like|want|need|always|never|only)\b",
        r"(?i)\bwe\s+(?:decided|agreed|chose|settled\s+on|went\s+with)\b",
        r"(?i)\b(?:always|never|don't|do\s+not|must|should\s+(?:always|never))\b.{0,40}\b(?:run|use|test|commit|push|deploy|edit|write|read|call|skip)",
        // Explicit blockers and errors.
        r"(?i)\b(?:error|fail(?:ed|s|ure)?|panic(?:ked)?|stuck|blocked|broken|crashed?|exception)\b",
        r"(?i)\b(?:can(?:'t|not)|unable\s+to|doesn't\s+work|not\s+working)\b",
        // Completion signals — implies the session accomplished
        // something worth recording.
        r"(?i)\b(?:done|finished|completed|implemented|fixed|shipped|landed|merged)\b",
        // Questions the user asked — "pending_user_asks" will
        // surface these.
        r"(?i)^(?:\s*)(?:how\s+do|why\s+(?:did|does|is)|can\s+you|could\s+you|what\s+(?:does|happens|if))\b",
        // Explicit code / file activity.
        r"(?i)\b(?:refactor|migrate|rewrite|rename|delete|create|add\s+(?:a|an|the)\s+(?:function|fn|method|struct|enum|module|file|test))\b",
        r"(?i)\.(?:rs|py|ts|tsx|js|jsx|go|c|cc|cpp|h|hpp|md|toml|yaml|yml|json|sh|html|css)\b",
        // Diagnostic keywords common in debug sessions.
        r"(?i)\b(?:compil(?:e|ed|er|ing)|linker|rustc|cargo|npm|pytest|tests?\s+(?:pass|fail))\b",
    ];
    RegexSet::new(raw).expect("prefilter SIGNAL_SET failed to compile")
});

/// Verdict from the pre-filter. Carries enough context that the
/// caller can log WHY a journal was skipped without a second pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Verdict {
    /// Proceed with the LLM summarization. The slice carries
    /// signal worth preserving.
    Proceed {
        /// Ordered list of pattern-category labels that fired.
        /// Empty when the only reason to proceed was min-char.
        signals: Vec<&'static str>,
        /// True when the "substantive length" fallback fired
        /// regardless of signal-regex hits. Useful for log
        /// observability ("journaled because long, not because
        /// signal").
        length_fallback: bool,
    },
    /// Skip the LLM call. The slice is chitchat-level and not
    /// worth the tokens.
    Skip { reason: &'static str },
}

/// Human-readable labels paired with the pattern indices above.
/// Kept in sync manually — there's only 10 patterns so the
/// coupling is cheap, and indirecting through a Vec adds no
/// safety since RegexSet returns raw indices anyway.
const SIGNAL_LABELS: &[&str] = &[
    "preference",
    "team-decision",
    "standing-rule",
    "error",
    "incapability",
    "completion",
    "user-question",
    "code-activity",
    "file-mention",
    "toolchain-diagnostic",
];

/// Run the pre-filter. Pure: no I/O, no allocations beyond the
/// returned Verdict.
pub(super) fn classify(history: &[ChatMessage]) -> Verdict {
    // Empty slice — trivially skip.
    if history.is_empty() {
        return Verdict::Skip {
            reason: "empty history slice",
        };
    }

    // Any ToolCall block anywhere => the agent actually did work,
    // unconditionally worth journaling. Cheaper than regex.
    let has_tool_activity = history.iter().any(|msg| {
        msg.content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolCall { .. }))
    });

    // Collect all text content into one big string for the regex
    // sweep. Allocation cost < regex cost, and RegexSet prefers
    // one scan over many. Also accumulate non-whitespace char
    // count for the min-substance check.
    let mut corpus = String::with_capacity(4_096);
    let mut nonws_chars = 0usize;
    for msg in history.iter() {
        for block in msg.content.iter() {
            match block {
                ContentBlock::Text { text } => {
                    corpus.push_str(text);
                    corpus.push('\n');
                    nonws_chars += text.chars().filter(|c| !c.is_whitespace()).count();
                }
                ContentBlock::Thinking { thinking, .. } => {
                    corpus.push_str(thinking);
                    corpus.push('\n');
                    nonws_chars += thinking.chars().filter(|c| !c.is_whitespace()).count();
                }
                ContentBlock::ToolResult { content, .. } => {
                    corpus.push_str(content);
                    corpus.push('\n');
                    nonws_chars += content.chars().filter(|c| !c.is_whitespace()).count();
                }
                ContentBlock::ToolCall { .. }
                | ContentBlock::Image { .. }
                | ContentBlock::EncryptedReasoning { .. } => {
                    // Tool-call args don't add signal for the
                    // pre-filter — we already counted the block
                    // via `has_tool_activity`.
                }
            }
        }
    }

    let matches: Vec<usize> = SIGNAL_SET.matches(&corpus).iter().collect();
    let signals: Vec<&'static str> = matches
        .iter()
        .filter_map(|&i| SIGNAL_LABELS.get(i).copied())
        .collect();

    if has_tool_activity {
        // Even if no signal regex fires, tool activity means we
        // want the journal. Label it for observability.
        let mut s = signals;
        if !s.contains(&"tool-activity") {
            s.insert(0, "tool-activity");
        }
        return Verdict::Proceed {
            signals: s,
            length_fallback: false,
        };
    }

    if !signals.is_empty() {
        return Verdict::Proceed {
            signals,
            length_fallback: false,
        };
    }

    if nonws_chars >= MIN_CHARS_FOR_SUBSTANCE {
        return Verdict::Proceed {
            signals: vec![],
            length_fallback: true,
        };
    }

    Verdict::Skip {
        reason: "no signal regexes fired and slice below substance threshold",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ChatMessage, ContentBlock, Role};

    fn u(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.into(),
            }],
        }
    }
    fn a(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: text.into(),
            }],
        }
    }

    #[test]
    fn empty_history_skipped() {
        matches!(classify(&[]), Verdict::Skip { .. });
    }

    #[test]
    fn chitchat_is_skipped() {
        let h = vec![u("thanks"), a("you're welcome")];
        match classify(&h) {
            Verdict::Skip { .. } => {}
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    #[test]
    fn preference_triggers_proceed() {
        let h = vec![u("I prefer using cargo test --lib always")];
        match classify(&h) {
            Verdict::Proceed { signals, length_fallback } => {
                assert!(!length_fallback);
                assert!(signals.contains(&"preference"));
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn team_decision_triggers_proceed() {
        let h = vec![u("we decided to use 12-section template")];
        match classify(&h) {
            Verdict::Proceed { signals, .. } => {
                assert!(signals.contains(&"team-decision"));
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn error_triggers_proceed() {
        let h = vec![a("build failed: cannot find type")];
        match classify(&h) {
            Verdict::Proceed { signals, .. } => {
                assert!(signals.contains(&"error") || signals.contains(&"incapability"));
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn completion_triggers_proceed() {
        let h = vec![a("fixed the oauth bug, tests pass")];
        match classify(&h) {
            Verdict::Proceed { signals, .. } => {
                assert!(signals.contains(&"completion"));
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn user_question_triggers_proceed() {
        let h = vec![u("how do I add a new migration?")];
        match classify(&h) {
            Verdict::Proceed { signals, .. } => {
                assert!(signals.contains(&"user-question"));
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn file_mention_triggers_proceed() {
        let h = vec![a("see src/repl/journal.rs for context")];
        match classify(&h) {
            Verdict::Proceed { signals, .. } => {
                assert!(signals.contains(&"file-mention"));
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn toolchain_keyword_triggers_proceed() {
        let h = vec![a("cargo build: compiling sidekar v2.5.38")];
        match classify(&h) {
            Verdict::Proceed { signals, .. } => {
                assert!(signals.contains(&"toolchain-diagnostic"));
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn tool_activity_always_proceeds_even_without_signal_words() {
        // No regex would fire on these texts; but the ToolCall
        // block alone is enough to journal.
        let h = vec![ChatMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "ok".into(),
                },
                ContentBlock::ToolCall {
                    id: "t-1".into(),
                    name: "Read".into(),
                    arguments: serde_json::json!({"path": "x"}),
                },
            ],
        }];
        match classify(&h) {
            Verdict::Proceed { signals, length_fallback } => {
                assert!(!length_fallback);
                assert!(signals.contains(&"tool-activity"));
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn long_substantive_slice_proceeds_via_length_fallback() {
        // Build a slice that has zero signal-regex hits but exceeds
        // the character threshold. Use content that avoids every
        // trigger — "apple" repeated.
        let text = "apple ".repeat(200); // 1000+ non-ws chars
        let h = vec![u(&text)];
        match classify(&h) {
            Verdict::Proceed { signals, length_fallback } => {
                assert!(length_fallback, "expected length fallback to fire");
                assert!(signals.is_empty(), "expected no signals, got {signals:?}");
            }
            other => panic!("expected Proceed via length, got {other:?}"),
        }
    }

    #[test]
    fn short_slice_without_signal_is_skipped() {
        // Under the 800-char threshold and no signals.
        let h = vec![u("apple banana cherry"), a("date elderberry fig")];
        match classify(&h) {
            Verdict::Skip { .. } => {}
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    #[test]
    fn signal_and_tool_activity_both_reported() {
        // ToolCall plus a preference in text — tool-activity must
        // be prepended, signal labels also included.
        let h = vec![ChatMessage {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "I prefer cargo test --lib".into(),
                },
                ContentBlock::ToolCall {
                    id: "t-2".into(),
                    name: "Bash".into(),
                    arguments: serde_json::json!({"command": "ls"}),
                },
            ],
        }];
        match classify(&h) {
            Verdict::Proceed { signals, .. } => {
                assert_eq!(signals[0], "tool-activity");
                assert!(signals.contains(&"preference"));
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }
}
