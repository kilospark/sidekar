use super::*;

#[test]
fn test_strip_ansi() {
    assert_eq!(strip_ansi(b"\x1b[32mhello\x1b[0m"), "hello");
    assert_eq!(strip_ansi(b"plain text"), "plain text");
    assert_eq!(strip_ansi(b"\x1b]0;title\x07rest"), "rest");
}

#[test]
fn test_classify_lines() {
    assert_eq!(classify_line(""), LineKind::Empty);
    assert_eq!(classify_line("hello world"), LineKind::Text);
    assert_eq!(classify_line("⏎ Read src/main.rs"), LineKind::ToolHeader);
    assert_eq!(classify_line("+added line"), LineKind::DiffAdd);
    assert_eq!(classify_line("-removed line"), LineKind::DiffRemove);
    assert_eq!(classify_line("@@ -1,3 +1,4 @@"), LineKind::DiffMeta);
    assert_eq!(classify_line("```rust"), LineKind::CodeFence);
    assert_eq!(classify_line("  indented output"), LineKind::ToolOutput);
}

#[test]
fn test_parser_text_block() {
    let mut parser = EventParser::new();
    let events = parser.feed(b"hello world\nmore text\n\n");
    assert_eq!(events.len(), 1);
    assert!(
        matches!(&events[0], AgentEvent::Text { content } if content == "hello world\nmore text")
    );
}

#[test]
fn test_parser_code_block() {
    let mut parser = EventParser::new();
    let events = parser.feed(b"```rust\nfn main() {}\n```\n");
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], AgentEvent::Code { language, content }
        if language == "rust" && content == "fn main() {}"));
}

#[test]
fn test_parser_diff() {
    let mut parser = EventParser::new();
    let events = parser.feed(b"@@ -1,3 +1,4 @@\n-old\n+new\n\n");
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], AgentEvent::Diff { .. }));
}

#[test]
fn test_parser_tool_header() {
    let mut parser = EventParser::new();
    let events = parser.feed(b"\xe2\x8f\x8e Read src/main.rs\n\n");
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], AgentEvent::ToolCall { tool, input }
        if tool == "Read" && input == "src/main.rs"));
}
