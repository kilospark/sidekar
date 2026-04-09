use super::*;

const SAMPLE: &str = r#"# Project Title

Some intro text here.

## Getting Started

Install with `cargo install`.

### Prerequisites

You need Rust 1.70+.

## API Reference

The main entry point is `run()`.

### Endpoints

- GET /health
- POST /data
"#;

#[test]
fn extracts_headings() {
    let headings = extract_headings(SAMPLE);
    assert_eq!(headings.len(), 5);
    assert_eq!(headings[0].text, "Project Title");
    assert_eq!(headings[0].level, 1);
    assert_eq!(headings[1].text, "Getting Started");
    assert_eq!(headings[1].level, 2);
    assert_eq!(headings[2].text, "Prerequisites");
    assert_eq!(headings[2].level, 3);
    assert_eq!(headings[3].text, "API Reference");
    assert_eq!(headings[3].level, 2);
    assert_eq!(headings[4].text, "Endpoints");
    assert_eq!(headings[4].level, 3);
}

#[test]
fn builds_sections() {
    let sections = build_sections(SAMPLE);
    assert_eq!(sections.len(), 5);
    assert!(sections[0].body.contains("Some intro text"));
    assert!(sections[1].body.contains("cargo install"));
    assert!(sections[2].body.contains("Rust 1.70"));
}

#[test]
fn searches_sections() {
    let hits = search_sections(SAMPLE, "cargo", "test.md");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].heading, "Getting Started");
    assert!(hits[0].context.contains("cargo install"));
}

#[test]
fn searches_multi_term() {
    let hits = search_sections(SAMPLE, "GET health", "test.md");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].heading, "Endpoints");
}

#[test]
fn finds_section_by_name() {
    let sections = build_sections(SAMPLE);
    let found = sections
        .iter()
        .find(|s| s.heading.text.to_lowercase().contains("prerequisites"));
    assert!(found.is_some());
    assert!(found.unwrap().body.contains("Rust 1.70"));
}
