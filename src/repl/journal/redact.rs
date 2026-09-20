//! Pattern-based credential redaction for the journaling pipeline.
//!
//! Runs on the history slice *before* `prompt::format_prompt` builds the user
//! body for the summarizer. Defense at the boundary beats "please don't log
//! secrets" in the prompt alone.
//!
//! Covers common token shapes: OpenAI (`sk-`), Anthropic (`sk-ant-`), Google
//! (`AIza`), AWS (`AKIA…` / contextual 40-char secrets), GitHub PAT variants,
//! Slack `xox`-prefixed tokens, JWT/Bearer lines, URL `api_key=` / `token=`,
//! and noisy `.env`-style `PASSWORD=`/`SECRET=`/`TOKEN=` lines.
//!
//! Skips indiscriminate high-entropy scrubbing (would mangle hashes, commit
//! IDs, etc.). Output-side scanning lives in the threat-scanner module; this
//! file is input-side only.

use std::sync::LazyLock;

use regex::{Regex, RegexSet};

/// The literal replacement string. Matches the convention in
/// src/commands/kv.rs so tooling that greps for redaction finds
/// both sites.
pub(super) const REDACTED: &str = "[REDACTED]";

/// Compiled once, reused for every redaction call. `LazyLock` over
/// `OnceLock` because we need a single-expression initializer that
/// can't fail — the patterns are compile-time constants, so an
/// `.unwrap()` is appropriate (a bad pattern is a build-time bug).
static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    // Order matters only for overlapping patterns — later replacements
    // act on the output of earlier ones. Each rule replaces the whole
    // match (no capture groups preserved) so callers can't accidentally
    // reintroduce part of the secret.
    let raw: &[&str] = &[
        // OpenAI. `sk-` followed by 20+ non-space, non-quote chars.
        // The bound prevents accidentally matching "sk-" followed by
        // a short word in ordinary prose.
        r"\bsk-(?:proj-)?[A-Za-z0-9_\-]{20,}",
        // Anthropic. `sk-ant-` is a narrower prefix; the rule above
        // catches the generic `sk-` case, but Anthropic uses a
        // longer key and we want to be explicit.
        r"\bsk-ant-[A-Za-z0-9_\-]{20,}",
        // Google / Firebase. 39-char key starting with `AIza`.
        r"\bAIza[0-9A-Za-z\-_]{35}",
        // AWS access key id. Exactly `AKIA` + 16 uppercase alnum.
        r"\bAKIA[0-9A-Z]{16}\b",
        // GitHub personal access tokens — all the prefix variants.
        r"\bghp_[A-Za-z0-9]{36,}",
        r"\bghs_[A-Za-z0-9]{36,}",
        r"\bgho_[A-Za-z0-9]{36,}",
        r"\bghu_[A-Za-z0-9]{36,}",
        r"\bghr_[A-Za-z0-9]{36,}",
        r"\bgithub_pat_[A-Za-z0-9_]{22,}",
        // Slack. Covers user, bot, workspace, legacy app tokens.
        r"\bxox[abpsr]-[A-Za-z0-9\-]{10,}",
        // JWT: three base64url segments separated by dots, starting
        // with `ey` (the typical `{"alg":...}` header). Accept the
        // third segment being empty (unsecured JWTs).
        r"\beyJ[A-Za-z0-9_\-]{10,}\.eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]*",
        // HTTP Authorization header value. Case-insensitive `Bearer`.
        r"(?i)Authorization:\s*Bearer\s+[A-Za-z0-9._\-]+",
        // URL parameters. `api_key=` or `token=` followed by a
        // non-`&` value. Covers query strings and curl --data rows.
        r#"(?i)\b(?:api[_-]?key|access[_-]?token|auth[_-]?token)=[^\s&"'<>]{8,}"#,
        // .env-style lines. Match from the start of a line (after
        // optional export), an all-caps env name ending in one of
        // the hot suffixes, then the rest of the line. The line-
        // anchor keeps this from gobbling prose that happens to
        // contain the word PASSWORD.
        r"(?im)^\s*(?:export\s+)?[A-Z][A-Z0-9_]*(?:PASSWORD|SECRET|TOKEN|APIKEY|API_KEY)\s*=\s*\S+.*$",
    ];
    raw.iter()
        .map(|p| Regex::new(p).expect("redactor pattern failed to compile"))
        .collect()
});

/// The same set, compiled as a `RegexSet` for a fast "does any
/// pattern match at all?" probe. Used in the shortcut path —
/// 99% of text in a coding session has zero secrets, and a
/// single set-match beats 15 sequential scans.
static PATTERN_SET: LazyLock<RegexSet> = LazyLock::new(|| {
    let raw: Vec<&str> = PATTERNS.iter().map(|r| r.as_str()).collect();
    RegexSet::new(raw).expect("redactor RegexSet failed to compile")
});

/// Scrub a string: return a new String with every pattern-match
/// replaced by `[REDACTED]`. If no patterns matched, return a
/// cheap clone — callers shouldn't have to branch on "was anything
/// changed" to use this.
///
/// Cost: one RegexSet scan to check for any hit (~microseconds for
/// typical turn content), plus one Regex::replace_all per hit
/// pattern. For a clean string, this is essentially free.
pub(super) fn redact(input: &str) -> String {
    if !PATTERN_SET.is_match(input) {
        return input.to_string();
    }
    let mut out = input.to_string();
    for (i, re) in PATTERNS.iter().enumerate() {
        // Only re-run the regex if the RegexSet said it matched —
        // RegexSet::matches gives per-pattern hits, but matches_iter
        // on RegexSet is trickier; explicit check-per-pattern is
        // clearer and still cheap.
        if PATTERN_SET.matches(&out).iter().any(|m| m == i) {
            out = re.replace_all(&out, REDACTED).into_owned();
        }
    }
    out
}

/// Redact in-place on every Text/Thinking/ToolResult ContentBlock
/// of a history slice, and on the string leaves of ToolCall args.
///
/// Tool arguments used to pass through untouched, on the reasoning that a
/// broken arg is worse than a leaked key and the tool had already seen the real
/// value. The first half of that holds; the second does not apply here. This
/// history is formatted into a prompt and sent to a model, so "the tool already
/// saw it" is not the question — whether a third party sees it is.
///
/// Redacting only string leaves keeps the objection satisfied: the JSON shape,
/// its keys, its numbers and its booleans are all untouched, and a string is
/// replaced by a string. Nothing can be structurally broken by a substitution
/// that cannot change a value's type.
///
/// Image and EncryptedReasoning blocks still pass through.
pub(super) fn redact_json_strings(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => {
            if PATTERN_SET.is_match(s) {
                *s = redact(s);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_json_strings(item);
            }
        }
        serde_json::Value::Object(map) => {
            // Keys are left alone: a key is a field name, not a secret, and
            // rewriting one would change the shape the tool expects.
            for (_, v) in map.iter_mut() {
                redact_json_strings(v);
            }
        }
        _ => {}
    }
}

pub(super) fn redact_history_in_place(history: &mut [crate::providers::ChatMessage]) {
    use crate::providers::ContentBlock;
    for msg in history.iter_mut() {
        for block in msg.content.iter_mut() {
            match block {
                ContentBlock::Text { text } => {
                    if PATTERN_SET.is_match(text) {
                        *text = redact(text);
                    }
                }
                ContentBlock::Thinking { thinking, .. } => {
                    if PATTERN_SET.is_match(thinking) {
                        *thinking = redact(thinking);
                    }
                }
                ContentBlock::Reasoning { text } => {
                    if PATTERN_SET.is_match(text) {
                        *text = redact(text);
                    }
                }
                ContentBlock::ToolResult {
                    content,
                    content_images,
                    ..
                } => {
                    if PATTERN_SET.is_match(content) {
                        *content = redact(content);
                    }
                    // Avoid huge pixel payloads in persisted journals.
                    content_images.clear();
                }
                ContentBlock::ToolCall { arguments, .. } => {
                    // String leaves only. The shape, keys, numbers and booleans
                    // are untouched, so nothing can be structurally broken.
                    redact_json_strings(arguments);
                }
                ContentBlock::Image { .. } | ContentBlock::EncryptedReasoning { .. } => {
                    // Deliberately untouched. See module doc.
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- positives: real-shaped strings that MUST be scrubbed --

    #[test]
    fn openai_key_redacted() {
        let s = "key is sk-proj-ABCDEFGHIJKLMNOPQRSTUV done.";
        let out = redact(s);
        assert!(out.contains(REDACTED));
        assert!(!out.contains("sk-proj-ABCDEFGHIJKLMNOPQRSTUV"));
        // Surrounding text preserved.
        assert!(out.starts_with("key is "));
        assert!(out.ends_with(" done."));
    }

    #[test]
    fn anthropic_key_redacted() {
        let s = "export ANTHROPIC_API_KEY=sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUV";
        let out = redact(s);
        // .env-line rule AND sk-ant rule both match; result is
        // still redacted regardless of which rule won first.
        assert!(out.contains(REDACTED));
        assert!(!out.contains("sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUV"));
    }

    #[test]
    fn google_api_key_redacted() {
        let s = "curl https://api.example.com?key=AIzaSyABCDEFGHIJKLMNOPQRSTUVWXYZ123456789";
        let out = redact(s);
        assert!(out.contains(REDACTED));
        assert!(!out.contains("AIzaSy"));
    }

    #[test]
    fn aws_access_key_redacted() {
        let s = "AKIAIOSFODNN7EXAMPLE is the access key";
        let out = redact(s);
        assert!(out.contains(REDACTED));
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn github_pat_redacted() {
        let s = "token: ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ";
        let out = redact(s);
        assert!(out.contains(REDACTED));
        assert!(!out.contains("ghp_abc"));
    }

    #[test]
    fn github_fine_grained_pat_redacted() {
        let s = "header: github_pat_11ABCDEFGHIJK0123456789_abcdefghijklm";
        let out = redact(s);
        assert!(out.contains(REDACTED));
        assert!(!out.contains("github_pat_11"));
    }

    #[test]
    fn slack_token_redacted() {
        let s = "slack: xoxb-1234567890-0987654321-abcdefghij";
        let out = redact(s);
        assert!(out.contains(REDACTED));
        assert!(!out.contains("xoxb-"));
    }

    #[test]
    fn jwt_redacted() {
        let s = "Cookie: session=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJ";
        let out = redact(s);
        assert!(out.contains(REDACTED));
        assert!(!out.contains("eyJhbGciOiJIUzI1NiJ9"));
    }

    #[test]
    fn bearer_header_redacted() {
        let s = "HTTP request:\nAuthorization: Bearer abc123.def456.ghi789\n(body)";
        let out = redact(s);
        assert!(out.contains(REDACTED));
        assert!(!out.contains("abc123.def456"));
        // Body preserved.
        assert!(out.contains("(body)"));
    }

    #[test]
    fn url_api_key_redacted() {
        let s = "GET /foo?api_key=SECRETVALUE12345 HTTP/1.1";
        let out = redact(s);
        assert!(out.contains(REDACTED));
        assert!(!out.contains("SECRETVALUE12345"));
    }

    #[test]
    fn env_password_line_redacted() {
        let s = "config:\nDATABASE_PASSWORD=supersecretpass123\nDEBUG=1\n";
        let out = redact(s);
        assert!(out.contains(REDACTED));
        assert!(!out.contains("supersecretpass123"));
        // Benign DEBUG line preserved.
        assert!(out.contains("DEBUG=1"));
    }

    // ---- negatives: strings that must NOT be scrubbed -----------

    #[test]
    fn ordinary_prose_untouched() {
        let s = "I fixed the bug in src/auth.rs — see commit abc123def.";
        let out = redact(s);
        assert_eq!(out, s);
    }

    #[test]
    fn commit_hash_not_mistaken_for_secret() {
        let s = "Reverting commit 7e967f4e9b3a1c2d5f6e8a0b4c9d2f1e3a5b7c8d.";
        let out = redact(s);
        assert_eq!(out, s);
    }

    #[test]
    fn uuid_not_mistaken_for_secret() {
        let s = "session id 550e8400-e29b-41d4-a716-446655440000 expired.";
        let out = redact(s);
        assert_eq!(out, s);
    }

    #[test]
    fn short_sk_prefix_not_overmatched() {
        // sk- followed by only a few chars — too short to be a real
        // key. The {20,} bound on the OpenAI rule protects this.
        let s = "sk-foo was the ticker symbol.";
        let out = redact(s);
        assert_eq!(out, s);
    }

    #[test]
    fn word_password_in_prose_not_redacted() {
        // The env-line rule anchors to line start + an all-caps name;
        // lowercase prose about passwords stays intact.
        let s = "I forgot my password, so I reset it.";
        let out = redact(s);
        assert_eq!(out, s);
    }

    // ---- integration: history slice mutation --------------------

    #[test]
    fn redact_history_in_place_scrubs_text_blocks() {
        use crate::providers::{ChatMessage, ContentBlock, Role};
        let mut history = vec![ChatMessage {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "use token ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ please".into(),
                },
                ContentBlock::Text {
                    text: "plain message".into(),
                },
            ],
        }];
        redact_history_in_place(&mut history);
        if let ContentBlock::Text { text } = &history[0].content[0] {
            assert!(text.contains(REDACTED));
            assert!(!text.contains("ghp_abc"));
        } else {
            panic!("expected text block");
        }
        // Second block untouched (no pattern match, no allocation).
        if let ContentBlock::Text { text } = &history[0].content[1] {
            assert_eq!(text, "plain message");
        }
    }

    #[test]
    fn redact_history_scrubs_tool_results_and_thinking() {
        use crate::providers::{ChatMessage, ContentBlock, Role};
        let mut history = vec![ChatMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: "the key sk-proj-ABCDEFGHIJKLMNOPQRSTUV is in env".into(),
                    signature: String::new(),
                },
                ContentBlock::ToolResult {
                    tool_use_id: "t-1".into(),
                    content: "Authorization: Bearer tok_12345.xyz".into(),
                    is_error: false,
                    content_images: vec![],
                },
            ],
        }];
        redact_history_in_place(&mut history);
        if let ContentBlock::Thinking { thinking, .. } = &history[0].content[0] {
            assert!(thinking.contains(REDACTED));
        } else {
            panic!("expected thinking block");
        }
        if let ContentBlock::ToolResult { content, .. } = &history[0].content[1] {
            assert!(content.contains(REDACTED));
        }
    }

    #[test]
    fn redact_history_scrubs_secrets_inside_tool_call_args() {
        // Was: tool args passed through untouched, so a token typed straight
        // into a Bash command survived into a prompt sent to a model. The old
        // reasoning — the tool already saw the real args — answers the wrong
        // question; what matters here is whether a third party sees it.
        use crate::providers::{ChatMessage, ContentBlock, Role};
        let mut history = vec![ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall {
                id: "t-1".into(),
                name: "Bash".into(),
                arguments: serde_json::json!({
                    "command": "echo ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ"
                }),
                thought_signature: None,
            }],
        }];
        redact_history_in_place(&mut history);
        if let ContentBlock::ToolCall { arguments, .. } = &history[0].content[0] {
            let cmd = arguments["command"].as_str().expect("still a string");
            assert!(!cmd.contains("ghp_abc"), "the secret must not survive");
            assert!(cmd.contains(REDACTED));
            assert!(
                cmd.starts_with("echo "),
                "the rest of the command is intact"
            );
        } else {
            panic!("expected tool call");
        }
    }

    #[test]
    fn redacting_args_cannot_break_their_structure() {
        // The original objection was that a broken arg is worse than a leaked
        // key. Only string leaves are touched, so keys, numbers, booleans,
        // nulls and nesting all survive and every value keeps its type.
        let mut v = serde_json::json!({
            "ghp_key_name_is_not_a_secret": "ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ",
            "count": 42,
            "enabled": true,
            "missing": null,
            "nested": {"deep": ["ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ", 7]}
        });
        redact_json_strings(&mut v);
        assert_eq!(v["count"], 42);
        assert_eq!(v["enabled"], true);
        assert!(v["missing"].is_null());
        // The key itself is a field name, not a secret, and must not be rewritten.
        assert!(v.get("ghp_key_name_is_not_a_secret").is_some());
        assert!(
            v["ghp_key_name_is_not_a_secret"]
                .as_str()
                .unwrap()
                .contains(REDACTED)
        );
        assert!(v["nested"]["deep"][0].as_str().unwrap().contains(REDACTED));
        assert_eq!(v["nested"]["deep"][1], 7);
    }

    #[test]
    fn clean_input_is_zero_allocation_fast_path() {
        // We can't easily assert "no allocation" from outside, but we
        // can at least confirm the output is string-equal to the
        // input for a clean case — that's the observable contract.
        let s = "line one\nline two\nline three\n";
        assert_eq!(redact(s), s);
    }
}
