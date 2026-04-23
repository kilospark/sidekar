//! Defensive parser for the LLM's journaling response.
//!
//! The prompt in `prompt.rs` tells the model to emit a single JSON
//! object with 12 specified fields. In practice models:
//!
//!   - Wrap the JSON in ```json ... ``` fences despite being told
//!     not to.
//!   - Prefix with "Here's the summary:" despite being told not to.
//!   - Omit fields they think are empty despite being told to
//!     include them as `[]` or `""`.
//!   - Use varying key casing (`active_task` vs `activeTask`).
//!   - Return a valid JSON object whose fields are strings when we
//!     asked for arrays (comma-separated strings).
//!   - Return truncated JSON if the output hit a length limit.
//!
//! This parser accepts all of the above and never panics. On hard
//! failures (no JSON object found at all) it returns a "degraded"
//! `StructuredJournal` containing the full raw text in
//! `critical_context` so a row still gets written and the session
//! doesn't silently lose data. The caller's store layer is append-
//! only and we'd rather have a bad-ish journal than nothing.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 12-section summary, the authoritative in-memory shape. Mirrors
/// the schema described in `prompt_schema.txt`. Serialized back to
/// `session_journals.structured_json` verbatim — downstream code
/// re-parses that JSON on injection, so the round-trip is covered.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct StructuredJournal {
    pub active_task: String,
    pub goal: String,
    pub constraints: Vec<String>,
    pub completed: Vec<String>,
    pub active_state: String,
    pub in_progress: Vec<String>,
    pub blocked: Vec<String>,
    pub decisions: Vec<String>,
    pub resolved_questions: Vec<String>,
    pub pending_user_asks: Vec<String>,
    pub relevant_files: Vec<String>,
    pub critical_context: String,
}

/// Extract the best-effort `StructuredJournal` from raw LLM output.
/// Never fails — on malformed input, produces a degraded but still-
/// insertable row. The `was_degraded` flag lets callers log/notice
/// parser failures without having to re-validate.
pub(super) struct ParseOutcome {
    pub journal: StructuredJournal,
    pub was_degraded: bool,
    /// When degraded, a short human-readable reason suitable for a
    /// log line. Empty string when parsing succeeded cleanly.
    pub reason: String,
}

/// Public entry point: try hard to parse, fall back gracefully.
pub(super) fn parse_response(raw: &str) -> ParseOutcome {
    // Stage 1: find a JSON object anywhere in the response. Strip
    // common wrappers (code fences, "Here's..." preambles).
    let candidate = match extract_json_object(raw) {
        Some(s) => s,
        None => {
            return degraded(
                raw,
                "no JSON object found in response",
            );
        }
    };

    // Stage 2: parse to a generic Value. If this fails, the JSON
    // is structurally broken (e.g. truncated mid-string from a
    // length-limited completion).
    let val: Value = match serde_json::from_str(&candidate) {
        Ok(v) => v,
        Err(e) => {
            return degraded(
                raw,
                &format!("JSON parse failed: {e}"),
            );
        }
    };

    // Stage 3: extract our known keys tolerantly. Unknown keys are
    // silently ignored. Missing keys default to empty. Arrays of
    // strings accept either a real array or a comma-separated
    // string (common degenerate response).
    let mut j = StructuredJournal::default();
    let mut hit_any_known_key = false;

    for (k, v) in val.as_object().into_iter().flatten() {
        let norm = normalize_key(k);
        match norm.as_str() {
            "active_task" => {
                j.active_task = extract_string(v);
                hit_any_known_key = true;
            }
            "goal" => {
                j.goal = extract_string(v);
                hit_any_known_key = true;
            }
            "constraints" => {
                j.constraints = extract_string_array(v);
                hit_any_known_key = true;
            }
            "completed" => {
                j.completed = extract_string_array(v);
                hit_any_known_key = true;
            }
            "active_state" => {
                j.active_state = extract_string(v);
                hit_any_known_key = true;
            }
            "in_progress" => {
                j.in_progress = extract_string_array(v);
                hit_any_known_key = true;
            }
            "blocked" => {
                j.blocked = extract_string_array(v);
                hit_any_known_key = true;
            }
            "decisions" => {
                j.decisions = extract_string_array(v);
                hit_any_known_key = true;
            }
            "resolved_questions" => {
                j.resolved_questions = extract_string_array(v);
                hit_any_known_key = true;
            }
            "pending_user_asks" => {
                j.pending_user_asks = extract_string_array(v);
                hit_any_known_key = true;
            }
            "relevant_files" => {
                j.relevant_files = extract_string_array(v);
                hit_any_known_key = true;
            }
            "critical_context" => {
                j.critical_context = extract_string(v);
                hit_any_known_key = true;
            }
            _ => {
                // Unknown key — ignore. Future schema additions
                // would land here; we want parsers from older
                // sidekars to accept newer journals gracefully.
            }
        }
    }

    // Stage 4: heuristic quality check. If the LLM returned a JSON
    // object but none of its keys matched our schema (e.g. it
    // invented its own format), treat as degraded and preserve
    // the raw text.
    if !hit_any_known_key {
        return degraded(raw, "JSON had no recognized journal fields");
    }

    ParseOutcome {
        journal: j,
        was_degraded: false,
        reason: String::new(),
    }
}

/// Compose a one-line headline suitable for `session_journals.
/// headline` and the /session teaser. Priority: active_task > goal
/// > first completed item > first constraint > "(empty summary)".
pub(super) fn extract_headline(j: &StructuredJournal) -> String {
    const HEADLINE_MAX: usize = 120;
    let src = if !j.active_task.is_empty() {
        j.active_task.as_str()
    } else if !j.goal.is_empty() {
        j.goal.as_str()
    } else if let Some(first) = j.completed.first() {
        first.as_str()
    } else if let Some(first) = j.constraints.first() {
        first.as_str()
    } else {
        "(empty summary)"
    };
    // Single line — newlines in active_task/goal would collapse
    // the /session rendering. Replace with spaces.
    let mut one_line: String = src.lines().next().unwrap_or("").trim().to_string();
    if one_line.is_empty() {
        one_line = "(empty summary)".to_string();
    }
    // UTF-8 safe char-count truncate. `.chars().count()` rather than
    // `.len()` to avoid splitting multi-byte chars.
    if one_line.chars().count() > HEADLINE_MAX {
        let truncated: String = one_line.chars().take(HEADLINE_MAX - 1).collect();
        format!("{truncated}…")
    } else {
        one_line
    }
}

// ---------- extractors --------------------------------------------

fn normalize_key(k: &str) -> String {
    // Accept camelCase by lower-snake-casing. Simple and sufficient
    // for the 12 fields we care about; no need to pull in heck.
    let mut out = String::with_capacity(k.len() + 4);
    for (i, ch) in k.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn extract_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        // An unexpected type — stringify. Better than dropping.
        other => other.to_string(),
    }
}

fn extract_string_array(v: &Value) -> Vec<String> {
    match v {
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            })
            .filter(|s| !s.is_empty())
            .collect(),
        // Comma-separated fallback. Some models like to return
        // constraints as "always X, always Y" instead of the
        // requested array.
        Value::String(s) => s
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
        Value::Null => Vec::new(),
        // Other shapes (object, number, bool) — unlikely, treat as
        // a single opaque entry so information isn't lost.
        other => vec![other.to_string()],
    }
}

/// Walk the raw text looking for a balanced `{...}` JSON object.
/// Trivial implementation tracking brace depth and respecting
/// strings + escapes — avoids the serde_json_path dependency for
/// one corner of the parser.
fn extract_json_object(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut i = 0;
    // Find the first '{'.
    while i < bytes.len() && bytes[i] != b'{' {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let start = i;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
        } else if b == b'"' {
            in_string = true;
        } else if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                // `i` is at the closing '}'. Inclusive end.
                return Some(raw[start..=i].to_string());
            }
        }
        i += 1;
    }
    None
}

fn degraded(raw: &str, reason: &str) -> ParseOutcome {
    // Preserve the whole raw response inside critical_context so
    // humans can still inspect what the model said. Truncate to a
    // sane size — the DB column has no length limit but we don't
    // want a 50 KB blob per bad row.
    const RAW_CAP: usize = 4_000;
    let mut ctx = String::from("[JOURNAL PARSE DEGRADED] ");
    ctx.push_str(reason);
    ctx.push_str("\nRaw model output:\n");
    if raw.chars().count() > RAW_CAP {
        let head: String = raw.chars().take(RAW_CAP).collect();
        ctx.push_str(&head);
        ctx.push_str("\n… [truncated]");
    } else {
        ctx.push_str(raw);
    }

    ParseOutcome {
        journal: StructuredJournal {
            critical_context: ctx,
            ..StructuredJournal::default()
        },
        was_degraded: true,
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_json_parses_all_fields() {
        let raw = r#"{
          "active_task": "finish oauth fix",
          "goal": "make codex login work",
          "constraints": ["use cargo test --lib", "always run bump-version.sh"],
          "completed": ["1. read src/providers/oauth.rs", "2. found state bug"],
          "active_state": "on branch main, no modified files",
          "in_progress": ["writing test"],
          "blocked": [],
          "decisions": ["send state for anthropic only"],
          "resolved_questions": ["is state required? no for openai"],
          "pending_user_asks": ["should we add logging?"],
          "relevant_files": ["src/providers/oauth.rs: where the fix lives"],
          "critical_context": "Anthropic returns 400 without state"
        }"#;
        let outcome = parse_response(raw);
        assert!(!outcome.was_degraded, "reason: {}", outcome.reason);
        let j = &outcome.journal;
        assert_eq!(j.active_task, "finish oauth fix");
        assert_eq!(j.constraints.len(), 2);
        assert_eq!(j.completed.len(), 2);
        assert_eq!(j.blocked, Vec::<String>::new());
        assert_eq!(j.critical_context, "Anthropic returns 400 without state");
    }

    #[test]
    fn code_fence_wrapping_is_stripped() {
        let raw = "Here's the summary:\n```json\n{\"active_task\":\"a\"}\n```\n";
        let outcome = parse_response(raw);
        assert!(!outcome.was_degraded);
        assert_eq!(outcome.journal.active_task, "a");
    }

    #[test]
    fn missing_fields_default_to_empty() {
        let raw = r#"{"active_task":"x"}"#;
        let outcome = parse_response(raw);
        assert!(!outcome.was_degraded);
        let j = &outcome.journal;
        assert_eq!(j.active_task, "x");
        assert_eq!(j.goal, "");
        assert!(j.constraints.is_empty());
        assert!(j.completed.is_empty());
    }

    #[test]
    fn camelcase_keys_are_accepted() {
        let raw = r#"{"activeTask":"a","pendingUserAsks":["q1"]}"#;
        let outcome = parse_response(raw);
        assert!(!outcome.was_degraded);
        assert_eq!(outcome.journal.active_task, "a");
        assert_eq!(outcome.journal.pending_user_asks, vec!["q1".to_string()]);
    }

    #[test]
    fn comma_separated_string_becomes_array() {
        let raw = r#"{"constraints": "use lib, run bump, no hand edits"}"#;
        let outcome = parse_response(raw);
        assert!(!outcome.was_degraded);
        assert_eq!(outcome.journal.constraints.len(), 3);
        assert_eq!(outcome.journal.constraints[0], "use lib");
    }

    #[test]
    fn completely_invalid_falls_back_to_degraded() {
        let raw = "I cannot summarize this conversation, sorry.";
        let outcome = parse_response(raw);
        assert!(outcome.was_degraded);
        assert!(outcome.reason.contains("no JSON object"));
        assert!(outcome.journal.critical_context.contains("DEGRADED"));
        assert!(outcome.journal.critical_context.contains("I cannot"));
    }

    #[test]
    fn truncated_json_falls_back_to_degraded() {
        // Missing closing brace — the balance walker never finds a
        // matching }, which is a valid rejection. Different reason
        // string from "parse failed," same outcome: degraded, not
        // a panic, raw text preserved.
        let raw = r#"{"active_task": "x", "goal": "trunc"#;
        let outcome = parse_response(raw);
        assert!(outcome.was_degraded);
        assert!(outcome.journal.critical_context.contains("trunc"));
    }

    #[test]
    fn structurally_valid_but_semantically_truncated_json_is_degraded() {
        // Syntactically complete (opens and closes balanced) but
        // the string value was cut mid-word before the closing
        // quote — the serde parse should reject.
        let raw = r#"{"active_task": "x", "goal": "trunc}"#;
        let outcome = parse_response(raw);
        assert!(outcome.was_degraded);
    }

    #[test]
    fn json_with_no_known_keys_is_degraded_not_silent() {
        let raw = r#"{"summary":"hello","author":"gpt"}"#;
        let outcome = parse_response(raw);
        assert!(outcome.was_degraded);
        assert!(outcome.reason.contains("no recognized"));
    }

    #[test]
    fn extract_headline_prefers_active_task() {
        let j = StructuredJournal {
            active_task: "fix oauth".into(),
            goal: "make login work".into(),
            ..Default::default()
        };
        assert_eq!(extract_headline(&j), "fix oauth");
    }

    #[test]
    fn extract_headline_falls_through_to_completed() {
        let j = StructuredJournal {
            completed: vec!["1. read src/auth.rs".into()],
            ..Default::default()
        };
        assert_eq!(extract_headline(&j), "1. read src/auth.rs");
    }

    #[test]
    fn extract_headline_truncates_long_strings() {
        let long = "x".repeat(500);
        let j = StructuredJournal {
            active_task: long,
            ..Default::default()
        };
        let h = extract_headline(&j);
        assert!(h.chars().count() <= 120);
        assert!(h.ends_with('…'));
    }

    #[test]
    fn json_inside_prose_is_extracted() {
        let raw = "Okay here we go:\n\nSummary:\n{\"active_task\":\"found it\"}\n\nEnd.";
        let outcome = parse_response(raw);
        assert!(!outcome.was_degraded);
        assert_eq!(outcome.journal.active_task, "found it");
    }

    #[test]
    fn nested_braces_in_strings_dont_break_balancing() {
        let raw = r#"{"active_task": "write fn() { body }", "goal":"test"}"#;
        let outcome = parse_response(raw);
        assert!(!outcome.was_degraded, "reason: {}", outcome.reason);
        assert_eq!(outcome.journal.active_task, "write fn() { body }");
        assert_eq!(outcome.journal.goal, "test");
    }

    #[test]
    fn escaped_quotes_dont_break_string_tracking() {
        let raw = r#"{"active_task":"say \"hi\" to the user","goal":""}"#;
        let outcome = parse_response(raw);
        assert!(!outcome.was_degraded);
        assert_eq!(outcome.journal.active_task, "say \"hi\" to the user");
    }
}
