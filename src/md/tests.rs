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

#[test]
fn stream_newline_gating() {
    let mut stream = MarkdownStream::new();
    stream.push("Hello **wor");
    assert!(stream.commit_complete_lines().is_empty());

    stream.push("ld**\n");
    let lines = stream.commit_complete_lines();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("world"));
}

#[test]
fn stream_finalize_partial() {
    let mut stream = MarkdownStream::new();
    stream.push("No newline here");
    assert!(stream.commit_complete_lines().is_empty());

    let lines = stream.finalize();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("No newline here"));
}

#[test]
fn previews_partial_line_before_newline() {
    let mut stream = MarkdownStream::new();
    stream.push("Hello **world");
    let preview = stream.preview_partial_line().expect("partial preview");
    assert!(preview.contains("Hello"));
    assert!(preview.contains("world"));
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

#[test]
fn streaming_table_withheld_until_separator() {
    let mut stream = MarkdownStream::new();

    // Header line arrives — should be withheld (no separator yet)
    stream.push("**Plays:**\n\n| Ticker | What |\n");
    let lines = stream.commit_complete_lines();
    assert!(!lines.is_empty());
    assert!(lines.iter().any(|l| l.contains("Plays:")));
    // The pipe line must NOT be committed yet
    assert!(!lines.iter().any(|l| l.contains("Ticker")));

    // Separator arrives — now table is confirmed, header + separator emitted
    stream.push("|--------|------|\n");
    let lines = stream.commit_complete_lines();
    assert!(lines.iter().any(|l| l.contains("Ticker")));
    assert!(lines.iter().any(|l| l.contains("---")));

    // Data row
    stream.push("| AGQ | 2x Silver |\n");
    let lines = stream.commit_complete_lines();
    assert!(lines.iter().any(|l| l.contains("AGQ")));

    // Finalize
    let lines = stream.finalize();
    assert!(lines.is_empty() || lines.iter().all(|l| l.is_empty()));
}
