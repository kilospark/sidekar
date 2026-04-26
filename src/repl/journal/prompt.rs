//! Pure formatter for the journaling summarization prompt.
//!
//! This module is deliberately side-effect-free:
//!   - No LLM calls.
//!   - No I/O.
//!   - No tokio, no threads, no DB reads.
//!   - No credential redaction yet — that lives in a dedicated
//!     module and runs as a pre-pass on the history slice before
//!     this formatter touches it.
//!
//! Given a slice of `ChatMessage`s (and optionally the structured
//! JSON of a previous journal, for iterative-update mode), produce
//! a single String that is the complete user-message-body for an
//! LLM call. The caller wraps that in whatever `system` + `user`
//! shape its provider wants.
//!
//! Why the 12-section hermes template, not pi-mono's 7-section:
//! see `context/hermes-memory-analysis-v2.md` — hermes's template
//! has "Active Task" as the top field (single most important for
//! resume), explicit Resolved/Pending split (prevents re-answering
//! resolved questions on resume), and an iterative-update mode
//! that preserves info across multiple passes. Pi-mono's shape is
//! fine for single-shot compaction but worse for the "write a
//! journal every 90s" pattern we have.
//!
//! Output expected from the LLM is a single JSON object (parsed by
//! `parse.rs`). This module's only job is to ask for it clearly.

use crate::providers::{ChatMessage, ContentBlock, Role};

/// How many chars of each content block to include verbatim before
/// we truncate with `… [truncated, N more chars]`. Keeps the prompt
/// bounded even when a message contains a giant file dump or tool
/// output. The summarizer doesn't need the full text — it needs
/// *signal*, and signal is usually in the first few KB.
const BLOCK_CHAR_CAP: usize = 4_000;

/// How many chars of the previous journal's structured_json to
/// include in iterative-update mode. The structured JSON is
/// typically 2-8 KB; we hand it back verbatim so the LLM can
/// preserve-and-extend rather than re-synthesize.
const PREVIOUS_JOURNAL_CAP: usize = 12_000;

/// Build the full prompt body.
///
/// * `history`: chronological slice of turns to summarize. Caller
///   is responsible for redacting credentials before passing in —
///   this module does NOT scrub, because doing so here would break
///   its no-I/O contract (redaction reads regex compilations etc.
///   better kept in one place).
/// * `previous_structured_json`: if `Some`, enables iterative-update
///   mode — the LLM is told to preserve the prior summary's
///   resolved/completed sections and add new deltas, rather than
///   starting from scratch. `None` on the first journal of a
///   session.
/// * `now_iso`: a pre-formatted timestamp like `2026-04-22T14:02Z`
///   so the LLM can anchor relative time phrases ("just now",
///   "an hour ago"). Passed in rather than read here to keep the
///   function pure.
pub(super) fn format_prompt(
    history: &[ChatMessage],
    previous_structured_json: Option<&str>,
    now_iso: &str,
) -> String {
    let mut out = String::with_capacity(8_192);

    // Top framing: one paragraph explaining the role. No system
    // prompt — we inject this as a user message because some
    // providers don't distinguish, and keeping it all in one place
    // makes testing trivial.
    out.push_str(include_str!("prompt_header.txt"));
    out.push_str("\nCurrent time: ");
    out.push_str(now_iso);
    out.push_str("\n\n");

    // Iterative-update mode vs fresh mode. The two prompts are
    // intentionally distinct — iterative mode tells the model to
    // preserve-and-extend, fresh mode tells it to synthesize from
    // scratch. Blending them was the hermes lesson: if you give
    // one prompt and sometimes attach a previous-summary, models
    // under-preserve because they weren't told it mattered.
    if let Some(prev) = previous_structured_json {
        out.push_str("## Mode: iterative update\n\n");
        out.push_str(
            "A previous journal for this session exists. Your job is to\n\
             UPDATE it: PRESERVE every entry in \"decisions\", \"constraints\",\n\
             \"resolved_questions\", \"relevant_files\" and \"completed\" that is\n\
             still relevant. APPEND new completed actions (continue numbering\n\
             from the previous list). MOVE items from \"in_progress\" to\n\
             \"completed\" when they finished. MOVE questions from \"pending\"\n\
             to \"resolved_questions\" when answered. UPDATE \"active_state\" to\n\
             reflect current state. The \"active_task\" field must reflect the\n\
             user's most recent unfulfilled request — this is the most\n\
             important field for continuity. DO NOT delete information\n\
             unless it is clearly obsolete.\n\n",
        );
        out.push_str("### Previous journal (to update):\n\n");
        out.push_str(&truncate_with_note(prev, PREVIOUS_JOURNAL_CAP));
        out.push_str("\n\n");
    } else {
        out.push_str("## Mode: fresh summary\n\n");
        out.push_str(
            "This is the first journal for this session. Summarize the\n\
             conversation below into the structured format described at the\n\
             end of this message.\n\n",
        );
    }

    // The conversation slice itself. Render each message as a
    // role-labeled block, flattening ContentBlocks to text. Tool
    // calls are shown as `[tool: name] {args}`; tool results as
    // `[result for id] ...`. Thinking blocks get a dim `[thinking]`
    // prefix. Images and encrypted-reasoning blocks are skipped —
    // they don't carry useful signal for a text summary.
    out.push_str("### Conversation slice\n\n");
    for (i, msg) in history.iter().enumerate() {
        let role = match msg.role {
            Role::User => "USER",
            Role::Assistant => "ASSISTANT",
        };
        out.push_str(&format!("--- Turn {} ({}) ---\n", i + 1, role));
        for block in &msg.content {
            render_block(block, &mut out);
        }
        out.push('\n');
    }

    // The schema instruction lives last so it's closest to the
    // response and hardest to forget. Repeating the field names
    // three times (here, in the template, and in the closing
    // reminder) is hermes's belt-and-suspenders pattern.
    out.push_str(include_str!("prompt_schema.txt"));
    out.push('\n');

    out
}

/// Render one ContentBlock as plain text into `out`. Writes a
/// trailing newline. Skips image/encrypted blocks which don't
/// surface in a journal summary.
fn render_block(block: &ContentBlock, out: &mut String) {
    match block {
        ContentBlock::Text { text } => {
            out.push_str(&truncate_with_note(text, BLOCK_CHAR_CAP));
            out.push('\n');
        }
        ContentBlock::Thinking { thinking, .. } => {
            out.push_str("[thinking] ");
            out.push_str(&truncate_with_note(thinking, BLOCK_CHAR_CAP));
            out.push('\n');
        }
        ContentBlock::ToolCall {
            name, arguments, ..
        } => {
            // Render args as a single-line JSON. Large tool args
            // (e.g. a full file's worth of content in an Edit)
            // get truncated. The fact that a tool was called is
            // usually more important than the exact arguments for
            // the summarizer's purposes.
            let args_str = serde_json::to_string(arguments).unwrap_or_else(|_| "{}".into());
            out.push_str("[tool-call ");
            out.push_str(name);
            out.push_str("] ");
            out.push_str(&truncate_with_note(&args_str, BLOCK_CHAR_CAP));
            out.push('\n');
        }
        ContentBlock::ToolResult {
            content, is_error, ..
        } => {
            if *is_error {
                out.push_str("[tool-error] ");
            } else {
                out.push_str("[tool-result] ");
            }
            out.push_str(&truncate_with_note(content, BLOCK_CHAR_CAP));
            out.push('\n');
        }
        ContentBlock::Image { .. } | ContentBlock::EncryptedReasoning { .. } => {
            // Deliberate no-op: these blocks can't be summarized
            // textually, and including a placeholder would encourage
            // the model to hallucinate their contents.
        }
    }
}

/// UTF-8-safe truncation with a human-readable suffix. Important:
/// slicing a String by bytes can split a multi-byte char and panic.
/// Finds the last char boundary at or before `max_chars` and cuts
/// there, then appends the truncation note.
fn truncate_with_note(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        return s.to_string();
    }
    let mut end = max_chars;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + 32);
    out.push_str(&s[..end]);
    out.push_str(&format!(" … [truncated, {} more chars]", s.len() - end));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ChatMessage, ContentBlock, Role};

    fn usr(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    fn asst(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    #[test]
    fn fresh_mode_emits_schema_and_conversation() {
        let hist = vec![
            usr("Fix the OAuth bug"),
            asst("I'll look at src/auth.rs first."),
        ];
        let out = format_prompt(&hist, None, "2026-04-22T12:00Z");

        // Top framing landed.
        assert!(out.contains("Current time: 2026-04-22T12:00Z"));
        // Fresh mode selected, iterative-update instructions absent.
        assert!(out.contains("## Mode: fresh summary"));
        assert!(!out.contains("## Mode: iterative update"));
        assert!(!out.contains("Previous journal"));
        // Both turns rendered with their roles.
        assert!(out.contains("--- Turn 1 (USER) ---"));
        assert!(out.contains("--- Turn 2 (ASSISTANT) ---"));
        assert!(out.contains("Fix the OAuth bug"));
        assert!(out.contains("I'll look at src/auth.rs first."));
        // Schema section present with all 12 fields.
        for field in [
            "active_task",
            "goal",
            "constraints",
            "completed",
            "active_state",
            "in_progress",
            "blocked",
            "decisions",
            "resolved_questions",
            "pending_user_asks",
            "relevant_files",
            "critical_context",
        ] {
            assert!(
                out.contains(field),
                "schema must mention field {field}"
            );
        }
    }

    #[test]
    fn iterative_mode_includes_previous_and_preserve_instruction() {
        let prev = r#"{"active_task":"refactor auth","completed":["1. read src/auth.rs"]}"#;
        let hist = vec![usr("continue from where you left off")];
        let out = format_prompt(&hist, Some(prev), "2026-04-22T13:00Z");
        assert!(out.contains("## Mode: iterative update"));
        assert!(out.contains("PRESERVE"));
        assert!(out.contains("refactor auth"));
        // Fresh mode text absent.
        assert!(!out.contains("## Mode: fresh summary"));
    }

    #[test]
    fn tool_calls_and_results_render_distinctively() {
        let hist = vec![ChatMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "Calling Read.".into(),
                },
                ContentBlock::ToolCall {
                    id: "t-1".into(),
                    name: "Read".into(),
                    arguments: serde_json::json!({"path": "src/auth.rs"}),
                    thought_signature: None,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "t-1".into(),
                    content: "fn login() { ... }".into(),
                    is_error: false,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "t-1".into(),
                    content: "file not found".into(),
                    is_error: true,
                },
            ],
        }];
        let out = format_prompt(&hist, None, "2026-04-22T14:00Z");
        assert!(out.contains("[tool-call Read]"));
        assert!(out.contains("src/auth.rs"));
        assert!(out.contains("[tool-result] fn login()"));
        assert!(out.contains("[tool-error] file not found"));
    }

    #[test]
    fn image_and_encrypted_blocks_are_skipped() {
        let hist = vec![ChatMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Image {
                    media_type: "image/png".into(),
                    data_base64: "IMAGEDATA".into(),
                    source_path: Some("/tmp/x.png".into()),
                },
                ContentBlock::EncryptedReasoning {
                    encrypted_content: "OPAQUE".into(),
                    summary: vec![],
                },
                ContentBlock::Text {
                    text: "visible text".into(),
                },
            ],
        }];
        let out = format_prompt(&hist, None, "0");
        assert!(out.contains("visible text"));
        // Neither the base64 data nor the opaque blob leaked in.
        assert!(!out.contains("IMAGEDATA"));
        assert!(!out.contains("OPAQUE"));
    }

    #[test]
    fn large_block_truncates_utf8_safely() {
        // Construct a string longer than BLOCK_CHAR_CAP with a
        // multi-byte character near the cap boundary.
        let mut s = "a".repeat(BLOCK_CHAR_CAP - 1);
        s.push('é'); // boundary char; bytes != chars here.
        s.push_str(&"b".repeat(500));
        let hist = vec![usr(&s)];
        let out = format_prompt(&hist, None, "0");
        // Truncation note present; no panic means UTF-8 boundary
        // logic worked.
        assert!(out.contains("[truncated,"));
    }

    #[test]
    fn empty_history_still_produces_valid_prompt() {
        let hist: Vec<ChatMessage> = vec![];
        let out = format_prompt(&hist, None, "0");
        // Framing and schema still present; conversation section
        // is empty but correctly labeled.
        assert!(out.contains("### Conversation slice"));
        assert!(out.contains("active_task"));
    }

    #[test]
    fn truncate_with_note_preserves_short_strings() {
        assert_eq!(truncate_with_note("hi", 100), "hi");
        let out = truncate_with_note(&"x".repeat(200), 50);
        assert!(out.starts_with(&"x".repeat(50)));
        assert!(out.contains("[truncated, 150 more chars]"));
    }
}
