use super::*;

#[test]
fn renders_heading() {
    let lines = render_markdown("## Hello\n");
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("Hello"));
    assert!(lines[0].contains(BOLD));
}

#[test]
fn renders_bold() {
    let lines = render_markdown("**bold text**\n");
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains(BOLD));
    assert!(lines[0].contains("bold text"));
}

#[test]
fn renders_inline_code() {
    let lines = render_markdown("use `foo` here\n");
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains(CYAN));
    assert!(lines[0].contains("`foo`"));
}

#[test]
fn renders_code_block() {
    let lines = render_markdown("```rust\nlet x = 1;\n```\n");
    assert!(lines.iter().any(|l| l.contains("rust")));
    assert!(lines.iter().any(|l| l.contains("let x = 1;")));
}

#[test]
fn renders_link_without_url_leak() {
    let lines = render_markdown("[docs](https://example.com/docs)\n");
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("docs"));
    assert!(!lines[0].contains("https://example.com/docs"));
}

#[test]
fn nested_link_styles_reapply_without_url_text() {
    let lines = render_markdown("**[docs](https://example.com/docs)** and `code`\n");
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("docs"));
    assert!(lines[0].contains("`code`"));
    assert!(!lines[0].contains("https://example.com/docs"));
}

/// Strip every ANSI CSI escape (`\x1b[...m`) so we can reason about the
/// printable width of rendered output in tests.
fn visible(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn table_columns_align_across_rows() {
    let md = "\
| Element | Markdown   | Purpose        |
|---------|------------|----------------|
| Heading | `## Title` | Section title  |
| Bold    | `**text**` | Strong emphasis |
| Italic  | `*text*`   | Emphasis        |
";
    let lines = render_markdown(md);
    assert_eq!(lines.len(), 5, "header + separator + 3 rows: {lines:?}");

    // Every pipe column must appear at the same visible offset on every row.
    let visible_lines: Vec<String> = lines.iter().map(|l| visible(l)).collect();
    let pipe_positions = |s: &str| -> Vec<usize> {
        s.char_indices()
            .filter(|(_, c)| *c == '|')
            .map(|(i, _)| i)
            .collect()
    };
    let first = pipe_positions(&visible_lines[0]);
    assert!(first.len() >= 4, "expected 4 pipes: {:?}", visible_lines[0]);
    for (i, line) in visible_lines.iter().enumerate().skip(1) {
        let ps = pipe_positions(line);
        assert_eq!(
            ps, first,
            "row {i} pipes at {ps:?} do not match header at {first:?} (line={line:?})",
        );
    }
}

#[test]
fn renders_table() {
    let md = "| Symbol | Price | Change |\n|--------|-------|--------|\n| AAPL | 150 | +2% |\n| GOOG | 2800 | -1% |\n";
    let lines = render_markdown(md);
    assert_eq!(lines.len(), 4);
    assert!(lines[0].contains("Symbol"));
    assert!(lines[0].contains("Price"));
    assert!(lines[0].contains("Change"));
    assert!(lines[1].contains("---"));
    assert!(lines[2].contains("AAPL"));
    assert!(lines[2].contains("150"));
    assert!(lines[3].contains("GOOG"));
    assert!(lines[3].contains("2800"));
}

// ---------------------------------------------------------------------------
// Streaming — block-boundary commit semantics
// ---------------------------------------------------------------------------

#[test]
fn no_commit_without_block_boundary() {
    let mut stream = MarkdownStream::new();
    stream.push("Hello **wor");
    assert!(stream.commit_complete_lines().is_empty());

    // A single newline inside a paragraph is NOT a safe commit boundary —
    // later deltas (emphasis close, hard break, etc.) can change the
    // interpretation of earlier text.
    stream.push("ld**\n");
    assert!(stream.commit_complete_lines().is_empty());
}

#[test]
fn commit_on_blank_line() {
    let mut stream = MarkdownStream::new();
    stream.push("First paragraph.\n");
    assert!(stream.commit_complete_lines().is_empty());

    stream.push("\n");
    let lines = stream.commit_complete_lines();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("First paragraph."));
}

#[test]
fn consecutive_paragraphs_get_blank_separator() {
    let mut stream = MarkdownStream::new();
    stream.push("First paragraph.\n\n");
    let first = stream.commit_complete_lines();
    assert_eq!(first.len(), 1);
    assert!(first[0].contains("First paragraph."));

    stream.push("Second paragraph.\n\n");
    let second = stream.commit_complete_lines();
    assert_eq!(second.len(), 2);
    assert_eq!(second[0], "", "expected blank separator between paragraphs");
    assert!(second[1].contains("Second paragraph."));
}

#[test]
fn finalize_partial_trailing_content() {
    let mut stream = MarkdownStream::new();
    stream.push("No newline here");
    assert!(stream.commit_complete_lines().is_empty());

    let lines = stream.finalize();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("No newline here"));
}

#[test]
fn emphasis_across_newlines_not_committed_early() {
    // The canonical bug: committing on every `\n` emits literal `**bold`
    // before the closing emphasis arrives, which can't be rewritten.
    let mut stream = MarkdownStream::new();
    stream.push("Text with **bold\n");
    assert!(
        stream.commit_complete_lines().is_empty(),
        "must not commit mid-emphasis",
    );

    stream.push("that continues**.\n\n");
    let lines = stream.commit_complete_lines();
    let joined: String = lines.concat();
    assert!(joined.contains("bold that continues"));
    assert!(joined.contains(BOLD));
    // The literal asterisks must not appear as content.
    assert!(
        !joined.contains("**bold"),
        "raw ** leaked into committed output: {joined:?}",
    );
}

#[test]
fn code_fence_not_committed_until_closed_and_blank() {
    let mut stream = MarkdownStream::new();
    stream.push("```rust\n");
    assert!(stream.commit_complete_lines().is_empty());

    stream.push("let x = 1;\n");
    assert!(
        stream.commit_complete_lines().is_empty(),
        "must wait for closing fence",
    );

    // Closing fence alone — commit point reached.
    stream.push("```\n");
    let lines = stream.commit_complete_lines();
    assert!(lines.iter().any(|l| l.contains("rust")));
    assert!(lines.iter().any(|l| l.contains("let x = 1;")));
}

#[test]
fn blank_line_inside_code_fence_is_not_a_boundary() {
    let mut stream = MarkdownStream::new();
    stream.push("```\n");
    assert!(stream.commit_complete_lines().is_empty());

    stream.push("first\n\nsecond\n");
    // The blank line is content INSIDE the fence — must not commit.
    assert!(
        stream.commit_complete_lines().is_empty(),
        "blank line inside fence must not commit",
    );

    stream.push("```\n");
    let lines = stream.commit_complete_lines();
    assert!(lines.iter().any(|l| l.contains("first")));
    assert!(lines.iter().any(|l| l.contains("second")));
}

#[test]
fn table_accumulates_until_blank_line() {
    let mut stream = MarkdownStream::new();
    stream.push("| Ticker | What |\n");
    assert!(stream.commit_complete_lines().is_empty());

    stream.push("|--------|------|\n");
    assert!(
        stream.commit_complete_lines().is_empty(),
        "table not yet complete",
    );

    stream.push("| AGQ | 2x Silver |\n");
    assert!(stream.commit_complete_lines().is_empty());

    stream.push("\n");
    let lines = stream.commit_complete_lines();
    assert!(lines.iter().any(|l| l.contains("Ticker")));
    assert!(lines.iter().any(|l| l.contains("AGQ")));
}

#[test]
fn table_flushed_on_finalize_without_trailing_blank() {
    let mut stream = MarkdownStream::new();
    stream.push("| A | B |\n|---|---|\n| 1 | 2 |\n");
    assert!(stream.commit_complete_lines().is_empty());

    let lines = stream.finalize();
    assert!(lines.iter().any(|l| l.contains("A")));
    assert!(lines.iter().any(|l| l.contains("1")));
}

#[test]
fn preview_shows_partial_inline_with_reset() {
    let mut stream = MarkdownStream::new();
    stream.push("Hello **world");
    let preview = stream.preview_partial_line().expect("partial preview");
    assert!(preview.contains("Hello"));
    assert!(preview.contains("world"));
    // ANSI hygiene — any open style must be terminated.
    assert!(
        preview.ends_with(RESET),
        "preview must terminate with RESET: {preview:?}",
    );
}

#[test]
fn preview_empty_when_fully_committed() {
    let mut stream = MarkdownStream::new();
    stream.push("Done.\n\n");
    let _ = stream.commit_complete_lines();
    assert!(stream.preview_partial_line().is_none());
}

#[test]
fn preview_shows_last_line_of_multiline_pending() {
    let mut stream = MarkdownStream::new();
    // A list mid-stream — no blank line yet, so nothing committed.
    stream.push("- one\n- two\n- thr");
    let preview = stream.preview_partial_line().expect("partial preview");
    // Show the most recent (trailing) rendered line so the user sees the
    // live edge of the stream, not a stale heading / earlier item.
    assert!(
        preview.contains("thr"),
        "expected trailing partial in preview: {preview:?}",
    );
}

#[test]
fn atx_heading_commits_immediately() {
    // ATX heading is a single-line leaf block — once the newline arrives,
    // nothing a future delta can add will change how the heading renders.
    let mut stream = MarkdownStream::new();
    stream.push("# Title\n");
    let lines = stream.commit_complete_lines();
    assert_eq!(lines.len(), 1, "heading should commit on its own newline");
    assert!(lines[0].contains("Title"));

    stream.push("\nBody paragraph.\n\n");
    let more = stream.commit_complete_lines();
    let joined: String = more.concat();
    assert!(joined.contains("Body paragraph."));
}

#[test]
fn paragraph_commits_when_heading_starts() {
    // A paragraph without a trailing blank line is still closed by the next
    // block starter — the ATX heading line unambiguously ends the paragraph.
    let mut stream = MarkdownStream::new();
    stream.push("Intro line.\n");
    assert!(
        stream.commit_complete_lines().is_empty(),
        "no block starter yet — paragraph buffered",
    );

    stream.push("## Next Section\n");
    let lines = stream.commit_complete_lines();
    let joined: String = lines.concat();
    assert!(joined.contains("Intro line."));
    assert!(joined.contains("Next Section"));
}

#[test]
fn paragraph_commits_when_fence_opens() {
    // A fenced code block opener also closes a preceding paragraph without
    // requiring a blank line. The opener itself stays buffered until close.
    let mut stream = MarkdownStream::new();
    stream.push("Prose.\n");
    assert!(stream.commit_complete_lines().is_empty());

    stream.push("```rust\n");
    let first = stream.commit_complete_lines();
    assert!(
        first.iter().any(|l| l.contains("Prose.")),
        "paragraph should commit when fence opens: {first:?}",
    );
    assert!(
        !first.iter().any(|l| l.contains("rust")),
        "fence body must not commit until the closing fence",
    );

    stream.push("let x = 1;\n```\n");
    let closed = stream.commit_complete_lines();
    assert!(closed.iter().any(|l| l.contains("let x = 1;")));
}

#[test]
fn indented_backticks_not_treated_as_fence_opener() {
    // 4+ leading spaces make the line an indented code block's content per
    // CommonMark, not a fence opener. The paragraph must stay buffered.
    let mut stream = MarkdownStream::new();
    stream.push("Paragraph one.\n    ```rust\n");
    assert!(
        stream.commit_complete_lines().is_empty(),
        "indented backticks must not be misclassified as a fence opener",
    );
}

#[test]
fn hash_without_trailing_space_not_heading() {
    // `#foo` (no space after the hash) is a paragraph per CommonMark, not
    // an ATX heading — so it should NOT be treated as a block starter.
    let mut stream = MarkdownStream::new();
    stream.push("Some text.\n#notaheading\n");
    assert!(
        stream.commit_complete_lines().is_empty(),
        "`#notaheading` is a paragraph continuation, not a heading",
    );
}

#[test]
fn blockquote_prefixes_every_rendered_line() {
    // Lazy continuation — source has "> " only on the first line but the
    // second line is still part of the same blockquote paragraph.
    let lines = render_markdown("> First line\nSecond line\n");
    assert_eq!(lines.len(), 2, "expected two rendered lines, got {lines:?}");
    assert!(lines[0].contains("> "));
    assert!(lines[0].contains("First line"));
    assert!(
        lines[1].contains("> "),
        "blockquote continuation must carry `> ` prefix: {:?}",
        lines[1],
    );
    assert!(lines[1].contains("Second line"));
}

#[test]
fn blockquote_explicit_continuation_prefixes_every_line() {
    let lines = render_markdown("> First line\n> Second line\n");
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("First line"));
    assert!(lines[1].contains("> "));
    assert!(lines[1].contains("Second line"));
}

#[test]
fn tight_bullet_list_renders_one_item_per_line() {
    let lines = render_markdown("- one\n- two\n- three\n");
    assert_eq!(
        lines.len(),
        3,
        "tight bullet list must produce one line per item, got {lines:?}",
    );
    assert!(lines[0].contains("one"));
    assert!(lines[1].contains("two"));
    assert!(lines[2].contains("three"));
}

#[test]
fn tight_ordered_list_renders_one_item_per_line() {
    let lines = render_markdown("1. First\n2. Second\n3. Third\n");
    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("First"));
    assert!(lines[1].contains("Second"));
    assert!(lines[2].contains("Third"));
}

#[test]
fn nested_list_indents_children_on_own_lines() {
    let lines = render_markdown("- outer\n  - inner\n- outer2\n");
    assert_eq!(
        lines.len(),
        3,
        "nested list must produce one line per item, got {lines:?}",
    );
    assert!(lines[0].contains("outer"));
    assert!(lines[1].contains("inner"));
    // Nested item has leading indent before the marker.
    assert!(lines[1].starts_with("  "));
    assert!(lines[2].contains("outer2"));
}

#[test]
fn tight_list_items_with_inline_formatting() {
    let lines = render_markdown("- **bold** item\n- *italic* item\n");
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains(BOLD));
    assert!(lines[0].contains("bold"));
    assert!(lines[1].contains(ITALIC));
    assert!(lines[1].contains("italic"));
}

#[test]
fn stream_reusable_after_finalize() {
    let mut stream = MarkdownStream::new();
    stream.push("Message one.\n\n");
    let _ = stream.commit_complete_lines();
    let _ = stream.finalize();

    // Fresh message — first commit should NOT be preceded by a blank.
    stream.push("Message two.\n\n");
    let lines = stream.commit_complete_lines();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("Message two."));
}
