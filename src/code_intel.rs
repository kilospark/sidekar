//! Lightweight code intelligence without a parser dependency.
//!
//! The command surface (`symbols`, `definition`, `references`, `structure`) stays
//! stable even when tree-sitter is not part of the build. Extraction is heuristic,
//! line-based, and intentionally conservative.

use std::fmt;
use std::path::Path;

use anyhow::{Result, anyhow, bail};
use regex::Regex;

mod block_util;
mod extractors;

pub(crate) use block_util::*;
pub(crate) use extractors::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Lang {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Go,
    Java,
    C,
    Cpp,
    Ruby,
    Bash,
    CSharp,
}

impl Lang {
    pub fn from_ext(ext: &str) -> Option<Lang> {
        match ext {
            "rs" => Some(Lang::Rust),
            "py" | "pyi" => Some(Lang::Python),
            "js" | "jsx" | "mjs" | "cjs" => Some(Lang::JavaScript),
            "ts" | "mts" | "cts" | "tsx" => Some(Lang::TypeScript),
            "go" => Some(Lang::Go),
            "java" => Some(Lang::Java),
            "c" | "h" => Some(Lang::C),
            "cc" | "cpp" | "cxx" | "hpp" | "hxx" | "hh" => Some(Lang::Cpp),
            "rb" => Some(Lang::Ruby),
            "sh" | "bash" | "zsh" => Some(Lang::Bash),
            "cs" => Some(Lang::CSharp),
            _ => None,
        }
    }

    pub fn from_path(path: &Path) -> Option<Lang> {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(Lang::from_ext)
    }
}

impl fmt::Display for Lang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::JavaScript => "javascript",
            Lang::TypeScript => "typescript",
            Lang::Go => "go",
            Lang::Java => "java",
            Lang::C => "c",
            Lang::Cpp => "cpp",
            Lang::Ruby => "ruby",
            Lang::Bash => "bash",
            Lang::CSharp => "csharp",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Trait,
    Type,
    Constant,
    Variable,
    Import,
    Module,
}

impl fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SymbolKind::Function => "fn",
            SymbolKind::Method => "method",
            SymbolKind::Class => "class",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Trait => "trait",
            SymbolKind::Type => "type",
            SymbolKind::Constant => "const",
            SymbolKind::Variable => "var",
            SymbolKind::Import => "import",
            SymbolKind::Module => "mod",
        })
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
    pub children: Vec<Symbol>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Reference {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub context: String,
}

pub fn extract_symbols(path: &Path) -> Result<Vec<Symbol>> {
    let lang = Lang::from_path(path).ok_or_else(|| anyhow!("unsupported file type"))?;
    let source = std::fs::read_to_string(path)?;
    let file_path = path.to_string_lossy().to_string();
    Ok(extract_symbols_from_source(&lang, &source, &file_path))
}

pub fn extract_symbols_recursive(root: &Path) -> Result<Vec<Symbol>> {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .build();

    let mut all = Vec::new();
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        if Lang::from_path(path).is_none() {
            continue;
        }
        match extract_symbols(path) {
            Ok(syms) => all.extend(syms),
            Err(_) => continue,
        }
    }
    Ok(all)
}

pub fn find_definition(root: &Path, name: &str) -> Result<Vec<Symbol>> {
    let all = extract_symbols_recursive(root)?;
    let matches: Vec<Symbol> = all
        .into_iter()
        .filter(|s| {
            s.kind != SymbolKind::Import
                && s.kind != SymbolKind::Variable
                && (s.name == name || s.children.iter().any(|c| c.name == name))
        })
        .flat_map(|s| {
            let child_matches: Vec<Symbol> = s
                .children
                .iter()
                .filter(|c| c.name == name)
                .cloned()
                .collect();
            if child_matches.is_empty() {
                vec![s]
            } else {
                child_matches
            }
        })
        .collect();
    Ok(matches)
}

pub fn find_references(root: &Path, name: &str) -> Result<Vec<Reference>> {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .build();

    let needle = Regex::new(&format!(r"\b{}\b", regex::escape(name)))?;
    let mut refs = Vec::new();
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        if Lang::from_path(path).is_none() {
            continue;
        }
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        for (idx, line) in source.lines().enumerate() {
            for mat in needle.find_iter(line) {
                refs.push(Reference {
                    file: path.to_string_lossy().to_string(),
                    line: idx as u32,
                    column: mat.start() as u32,
                    context: line.trim().to_string(),
                });
            }
        }
    }
    Ok(refs)
}

pub fn get_symbol_body(path: &Path, name: &str) -> Result<String> {
    let symbols = extract_symbols(path)?;
    let source = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = source.lines().collect();

    for sym in &symbols {
        if sym.name == name && sym.kind != SymbolKind::Import {
            return slice_lines(&lines, sym.line_start, sym.line_end);
        }
        for child in &sym.children {
            if child.name == name {
                return slice_lines(&lines, child.line_start, child.line_end);
            }
        }
    }

    bail!("symbol '{name}' not found in {}", path.display())
}

pub fn format_symbols(symbols: &[Symbol], root: &Path) -> String {
    let mut out = String::new();
    for s in symbols {
        if s.kind == SymbolKind::Import {
            continue;
        }
        let rel = pathdiff(root, &s.file);
        out.push_str(&format!(
            "{:<8} {:<30} {}:{}\n",
            s.kind,
            s.name,
            rel,
            s.line_start + 1,
        ));
        for child in &s.children {
            out.push_str(&format!(
                "  {:<6} {:<28} {}:{}\n",
                child.kind,
                child.name,
                rel,
                child.line_start + 1,
            ));
        }
    }
    out
}

pub fn format_references(refs: &[Reference], root: &Path) -> String {
    let mut out = String::new();
    for r in refs {
        let rel = pathdiff(root, &r.file);
        out.push_str(&format!(
            "{}:{}:{} {}\n",
            rel,
            r.line + 1,
            r.column + 1,
            r.context
        ));
    }
    out
}

fn slice_lines(lines: &[&str], start: u32, end: u32) -> Result<String> {
    let start = start as usize;
    let end = (end as usize + 1).min(lines.len());
    if start >= end || start >= lines.len() {
        bail!("symbol source range is invalid");
    }
    Ok(lines[start..end].join("\n"))
}

fn pathdiff(root: &Path, file: &str) -> String {
    let fp = Path::new(file);
    fp.strip_prefix(root)
        .unwrap_or(fp)
        .to_string_lossy()
        .to_string()
}
