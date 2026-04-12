//! Markdown document intelligence: outline, section extraction, search, and mapping.
//!
//! Pure pulldown-cmark parsing — no LLM, no embeddings, no network.
//! The prose counterpart of code_intel.rs.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::path::Path;

/// A heading in a markdown document.
#[derive(Debug, Clone)]
pub struct Heading {
    pub level: u8,    // 1–6
    pub text: String, // heading text (inline content flattened)
    pub line: usize,  // 1-based line number
    pub byte_start: usize,
}

/// A section: heading + body text until next same-or-higher-level heading.
#[derive(Debug, Clone)]
pub struct Section {
    pub heading: Heading,
    pub body: String,    // raw markdown lines under this heading
    pub line_end: usize, // 1-based line of last line in section
}

/// A search hit within a section.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub file: String,
    pub heading: String,
    pub heading_level: u8,
    pub line: usize,     // line of the match within the section
    pub context: String, // the matching line
}

// ─── Outline ────────────────────────────────────────────────────────────────

/// Extract all headings from a markdown file.
pub fn extract_outline(path: &Path) -> anyhow::Result<Vec<Heading>> {
    let source = std::fs::read_to_string(path)?;
    Ok(extract_headings(&source))
}

fn extract_headings(source: &str) -> Vec<Heading> {
    let opts = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    let mut headings = Vec::new();
    let mut in_heading: Option<(u8, usize, String)> = None;

    let parser = Parser::new_ext(source, opts).into_offset_iter();
    for (event, range) in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let line = byte_offset_to_line(source, range.start);
                in_heading = Some((heading_level_to_u8(level), line, String::new()));
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, line, text)) = in_heading.take() {
                    headings.push(Heading {
                        level,
                        text,
                        line,
                        byte_start: range.start,
                    });
                }
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some((_, _, ref mut text)) = in_heading {
                    text.push_str(&t);
                }
            }
            _ => {}
        }
    }
    headings
}

// ─── Section extraction ─────────────────────────────────────────────────────

/// Extract all sections from a markdown file.
pub fn extract_sections(path: &Path) -> anyhow::Result<Vec<Section>> {
    let source = std::fs::read_to_string(path)?;
    Ok(build_sections(&source))
}

fn build_sections(source: &str) -> Vec<Section> {
    let headings = extract_headings(source);
    if headings.is_empty() {
        return Vec::new();
    }

    let lines: Vec<&str> = source.lines().collect();
    let total_lines = lines.len();
    let mut sections = Vec::new();

    for (i, heading) in headings.iter().enumerate() {
        let body_start = heading.line; // line after heading (1-based, heading.line is the heading line)
        let body_end = headings[i + 1..]
            .iter()
            .find(|h| h.level <= heading.level)
            .map(|h| h.line - 1)
            .unwrap_or(total_lines);

        // Collect body lines (skip the heading line itself)
        let body = if body_start < total_lines {
            lines[body_start..body_end].join("\n").trim().to_string()
        } else {
            String::new()
        };

        sections.push(Section {
            heading: heading.clone(),
            body,
            line_end: body_end,
        });
    }
    sections
}

/// Find a section by heading text (case-insensitive substring match).
pub fn find_section(path: &Path, query: &str) -> anyhow::Result<Option<Section>> {
    let sections = extract_sections(path)?;
    let query_lower = query.to_lowercase();
    Ok(sections
        .into_iter()
        .find(|s| s.heading.text.to_lowercase().contains(&query_lower)))
}

/// Find a section by heading text across a directory of markdown files.
pub fn find_section_recursive(root: &Path, query: &str) -> anyhow::Result<Vec<(String, Section)>> {
    let query_lower = query.to_lowercase();
    let mut results = Vec::new();

    for path in walk_markdown_files(root)? {
        let sections = extract_sections(&path)?;
        for s in sections {
            if s.heading.text.to_lowercase().contains(&query_lower) {
                let rel = rel_path(root, &path);
                results.push((rel, s));
            }
        }
    }
    Ok(results)
}

// ─── Search ─────────────────────────────────────────────────────────────────

/// Search for a keyword across sections in a markdown file.
pub fn search_file(path: &Path, query: &str) -> anyhow::Result<Vec<SearchHit>> {
    let source = std::fs::read_to_string(path)?;
    let file_str = path.to_string_lossy().to_string();
    Ok(search_sections(&source, query, &file_str))
}

/// Search across all markdown files in a directory.
pub fn search_recursive(root: &Path, query: &str) -> anyhow::Result<Vec<SearchHit>> {
    let mut all_hits = Vec::new();
    for path in walk_markdown_files(root)? {
        let rel = rel_path(root, &path);
        let source = std::fs::read_to_string(&path)?;
        all_hits.extend(search_sections(&source, query, &rel));
    }
    Ok(all_hits)
}

fn search_sections(source: &str, query: &str, file: &str) -> Vec<SearchHit> {
    let query_lower = query.to_lowercase();
    let terms: Vec<&str> = query_lower.split_whitespace().collect();
    let lines: Vec<&str> = source.lines().collect();
    let headings = extract_headings(source);

    let mut hits = Vec::new();

    // For each line, find which heading it falls under
    for (line_idx, line) in lines.iter().enumerate() {
        let line_lower = line.to_lowercase();
        // All query terms must appear in the line
        if terms.iter().all(|t| line_lower.contains(t)) {
            // Find the enclosing heading
            let (heading_text, heading_level) = find_enclosing_heading(&headings, line_idx + 1);
            hits.push(SearchHit {
                file: file.to_string(),
                heading: heading_text,
                heading_level,
                line: line_idx + 1,
                context: line.trim().to_string(),
            });
        }
    }
    hits
}

fn find_enclosing_heading(headings: &[Heading], line_1based: usize) -> (String, u8) {
    let mut best = ("(top)".to_string(), 0u8);
    for h in headings {
        if h.line <= line_1based {
            best = (h.text.clone(), h.level);
        } else {
            break;
        }
    }
    best
}

// ─── Map ────────────────────────────────────────────────────────────────────

/// Multi-file heading overview.
pub struct FileMap {
    pub file: String,
    pub headings: Vec<Heading>,
}

pub fn map_directory(root: &Path) -> anyhow::Result<Vec<FileMap>> {
    let mut maps = Vec::new();
    for path in walk_markdown_files(root)? {
        let headings = extract_outline(&path)?;
        if !headings.is_empty() {
            let rel = rel_path(root, &path);
            maps.push(FileMap {
                file: rel,
                headings,
            });
        }
    }
    Ok(maps)
}

pub fn map_file(path: &Path) -> anyhow::Result<FileMap> {
    let headings = extract_outline(path)?;
    Ok(FileMap {
        file: path.to_string_lossy().to_string(),
        headings,
    })
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn heading_level_to_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn byte_offset_to_line(source: &str, offset: usize) -> usize {
    source[..offset].matches('\n').count() + 1
}

fn walk_markdown_files(root: &Path) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false) // include dotfiles like .claude/
        .git_ignore(true)
        .build();

    for entry in walker {
        let entry = entry?;
        let path = entry.path();
        if path.is_file()
            && let Some("md" | "mdx" | "markdown") = path.extension().and_then(|e| e.to_str())
        {
            files.push(path.to_path_buf());
        }
    }
    files.sort();
    Ok(files)
}

fn rel_path(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests;
