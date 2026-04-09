//! Lightweight code intelligence without a parser dependency.
//!
//! The command surface (`symbols`, `definition`, `references`, `structure`) stays
//! stable even when tree-sitter is not part of the build. Extraction is heuristic,
//! line-based, and intentionally conservative.

use std::fmt;
use std::path::Path;

use anyhow::{Result, anyhow, bail};
use regex::Regex;

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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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

fn extract_symbols_from_source(lang: &Lang, source: &str, file_path: &str) -> Vec<Symbol> {
    match lang {
        Lang::Rust => extract_rust_symbols(source, file_path),
        Lang::Python => extract_python_symbols(source, file_path),
        Lang::JavaScript | Lang::TypeScript => extract_js_like_symbols(lang, source, file_path),
        Lang::Go => extract_go_symbols(source, file_path),
        Lang::Java => extract_java_like_symbols(lang, source, file_path),
        Lang::C => extract_c_like_symbols(lang, source, file_path),
        Lang::Cpp => extract_cpp_symbols(source, file_path),
        Lang::Ruby => extract_ruby_symbols(source, file_path),
        Lang::Bash => extract_bash_symbols(source, file_path),
        Lang::CSharp => extract_java_like_symbols(lang, source, file_path),
    }
}

fn extract_rust_symbols(source: &str, file_path: &str) -> Vec<Symbol> {
    let lines: Vec<&str> = source.lines().collect();
    let use_re = Regex::new(r"^\s*use\s+(.+);").unwrap();
    let mod_re = Regex::new(r"^\s*mod\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let fn_re =
        Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)")
            .unwrap();
    let struct_re =
        Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let enum_re =
        Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let trait_re =
        Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?trait\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let type_re =
        Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?type\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let const_re =
        Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const|static)\s+([A-Za-z_][A-Za-z0-9_]*)")
            .unwrap();
    let macro_re = Regex::new(r"^\s*macro_rules!\s*([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let impl_re = Regex::new(
        r"^\s*impl(?:<[^>]+>)?\s+(?:(?:[A-Za-z_][A-Za-z0-9_<>:]*)\s+for\s+)?([A-Za-z_][A-Za-z0-9_]*)",
    )
    .unwrap();

    let mut out = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();

        if let Some(cap) = use_re.captures(trimmed) {
            out.push(simple_symbol(
                cap[1].trim(),
                SymbolKind::Import,
                file_path,
                i,
                i,
                Some(trimmed.trim_end_matches(';').to_string()),
                collect_line_comments(&lines, i, "///"),
            ));
            i += 1;
            continue;
        }

        if let Some(cap) = mod_re.captures(trimmed) {
            let end = brace_block_end(&lines, i).unwrap_or(i);
            out.push(simple_symbol(
                &cap[1],
                SymbolKind::Module,
                file_path,
                i,
                end,
                Some(trimmed.to_string()),
                collect_line_comments(&lines, i, "///"),
            ));
            i = end.saturating_add(1);
            continue;
        }

        if let Some(cap) = impl_re.captures(trimmed) {
            let end = brace_block_end(&lines, i).unwrap_or(i);
            let parent_name = cap[1].to_string();
            let children = extract_rust_impl_methods(&lines, file_path, i, end);
            out.push(
                simple_symbol(
                    &parent_name,
                    SymbolKind::Struct,
                    file_path,
                    i,
                    end,
                    Some(trimmed.to_string()),
                    collect_line_comments(&lines, i, "///"),
                )
                .with_children(children),
            );
            i = end.saturating_add(1);
            continue;
        }

        if let Some(sym) = capture_symbol(
            &lines,
            i,
            &[
                (&fn_re, SymbolKind::Function),
                (&struct_re, SymbolKind::Struct),
                (&enum_re, SymbolKind::Enum),
                (&trait_re, SymbolKind::Trait),
                (&type_re, SymbolKind::Type),
                (&const_re, SymbolKind::Constant),
                (&macro_re, SymbolKind::Function),
            ],
            file_path,
            "///",
        ) {
            let next = sym.line_end as usize + 1;
            out.push(sym);
            i = next;
            continue;
        }

        i += 1;
    }

    out
}

fn extract_rust_impl_methods(
    lines: &[&str],
    file_path: &str,
    start: usize,
    end: usize,
) -> Vec<Symbol> {
    let fn_re =
        Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)")
            .unwrap();
    let mut methods = Vec::new();
    let mut i = start.saturating_add(1);
    while i <= end && i < lines.len() {
        let trimmed = lines[i].trim_start();
        if let Some(cap) = fn_re.captures(trimmed) {
            let method_end = brace_block_end(lines, i).unwrap_or(i);
            methods.push(simple_symbol(
                &cap[1],
                SymbolKind::Method,
                file_path,
                i,
                method_end,
                Some(trimmed.to_string()),
                collect_line_comments(lines, i, "///"),
            ));
            i = method_end.saturating_add(1);
            continue;
        }
        i += 1;
    }
    methods
}

fn extract_python_symbols(source: &str, file_path: &str) -> Vec<Symbol> {
    let lines: Vec<&str> = source.lines().collect();
    let import_re = Regex::new(r"^\s*(?:from\s+\S+\s+import|import)\b(.+)").unwrap();
    let class_re = Regex::new(r"^(\s*)class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let fn_re = Regex::new(r"^(\s*)def\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();

    let mut out = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if let Some(cap) = import_re.captures(line) {
            out.push(simple_symbol(
                cap[1].trim(),
                SymbolKind::Import,
                file_path,
                i,
                i,
                Some(trimmed.to_string()),
                collect_hash_comments(&lines, i),
            ));
            i += 1;
            continue;
        }
        if let Some(cap) = class_re.captures(line) {
            let indent = cap[1].len();
            let end = python_block_end(&lines, i, indent).unwrap_or(i);
            out.push(simple_symbol(
                &cap[2],
                SymbolKind::Class,
                file_path,
                i,
                end,
                Some(trimmed.to_string()),
                collect_hash_comments(&lines, i),
            ));
            i = end.saturating_add(1);
            continue;
        }
        if let Some(cap) = fn_re.captures(line) {
            let indent = cap[1].len();
            let end = python_block_end(&lines, i, indent).unwrap_or(i);
            let kind = if indent > 0 {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            };
            out.push(simple_symbol(
                &cap[2],
                kind,
                file_path,
                i,
                end,
                Some(trimmed.to_string()),
                collect_hash_comments(&lines, i),
            ));
            i = end.saturating_add(1);
            continue;
        }
        i += 1;
    }
    out
}

fn extract_js_like_symbols(lang: &Lang, source: &str, file_path: &str) -> Vec<Symbol> {
    let lines: Vec<&str> = source.lines().collect();
    let import_re = Regex::new(r"^\s*(?:import\b.+|export\s+\{.+\}\s+from\b.+)").unwrap();
    let fn_re =
        Regex::new(r"^\s*(?:export\s+)?(?:async\s+)?function\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let class_re = Regex::new(r"^\s*(?:export\s+)?class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let var_re =
        Regex::new(r"^\s*(?:export\s+)?(?:const|let|var)\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let interface_re =
        Regex::new(r"^\s*(?:export\s+)?interface\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let type_re = Regex::new(r"^\s*(?:export\s+)?type\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let enum_re = Regex::new(r"^\s*(?:export\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();

    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if import_re.is_match(trimmed) {
            out.push(simple_symbol(
                trimmed,
                SymbolKind::Import,
                file_path,
                i,
                i,
                Some(trimmed.to_string()),
                collect_line_comments(&lines, i, "//"),
            ));
            continue;
        }
        if let Some(sym) = capture_symbol(
            &lines,
            i,
            &[
                (&fn_re, SymbolKind::Function),
                (&class_re, SymbolKind::Class),
                (&var_re, SymbolKind::Variable),
                (&interface_re, SymbolKind::Trait),
                (&type_re, SymbolKind::Type),
                (&enum_re, SymbolKind::Enum),
            ],
            file_path,
            "//",
        ) {
            out.push(sym);
        }
    }

    if matches!(lang, Lang::JavaScript) {
        out.retain(|s| {
            s.kind != SymbolKind::Trait && s.kind != SymbolKind::Enum && s.kind != SymbolKind::Type
        });
    }
    out
}

fn extract_go_symbols(source: &str, file_path: &str) -> Vec<Symbol> {
    let lines: Vec<&str> = source.lines().collect();
    let import_re = Regex::new(r"^\s*import\b").unwrap();
    let fn_re = Regex::new(r"^\s*func(?:\s*\([^)]*\))?\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let type_re = Regex::new(r"^\s*type\s+([A-Za-z_][A-Za-z0-9_]*)\s+(struct|interface)").unwrap();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if import_re.is_match(trimmed) {
            out.push(simple_symbol(
                trimmed,
                SymbolKind::Import,
                file_path,
                i,
                i,
                Some(trimmed.to_string()),
                collect_line_comments(&lines, i, "//"),
            ));
            continue;
        }
        if let Some(cap) = fn_re.captures(trimmed) {
            let end = brace_block_end(&lines, i).unwrap_or(i);
            out.push(simple_symbol(
                &cap[1],
                if trimmed.starts_with("func (") {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                },
                file_path,
                i,
                end,
                Some(trimmed.to_string()),
                collect_line_comments(&lines, i, "//"),
            ));
            continue;
        }
        if let Some(cap) = type_re.captures(trimmed) {
            let kind = if &cap[2] == "interface" {
                SymbolKind::Trait
            } else {
                SymbolKind::Struct
            };
            let end = brace_block_end(&lines, i).unwrap_or(i);
            out.push(simple_symbol(
                &cap[1],
                kind,
                file_path,
                i,
                end,
                Some(trimmed.to_string()),
                collect_line_comments(&lines, i, "//"),
            ));
        }
    }
    out
}

fn extract_java_like_symbols(lang: &Lang, source: &str, file_path: &str) -> Vec<Symbol> {
    let lines: Vec<&str> = source.lines().collect();
    let import_re = Regex::new(r"^\s*(?:import|using)\b.+").unwrap();
    let class_re = Regex::new(r"^\s*(?:public|private|protected|internal|static|final|abstract|\s)*class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let interface_re = Regex::new(r"^\s*(?:public|private|protected|internal|static|abstract|\s)*(?:interface)\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let enum_re = Regex::new(
        r"^\s*(?:public|private|protected|internal|static|\s)*enum\s+([A-Za-z_][A-Za-z0-9_]*)",
    )
    .unwrap();
    let method_re = Regex::new(r"^\s*(?:public|private|protected|internal|static|final|abstract|virtual|override|async|\s)+[A-Za-z_][A-Za-z0-9_<>,\[\]?]*\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap();

    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if import_re.is_match(trimmed) {
            out.push(simple_symbol(
                trimmed,
                SymbolKind::Import,
                file_path,
                i,
                i,
                Some(trimmed.to_string()),
                collect_line_comments(&lines, i, "//"),
            ));
            continue;
        }
        if let Some(sym) = capture_symbol(
            &lines,
            i,
            &[
                (&class_re, SymbolKind::Class),
                (&interface_re, SymbolKind::Trait),
                (&enum_re, SymbolKind::Enum),
                (&method_re, SymbolKind::Function),
            ],
            file_path,
            if matches!(lang, Lang::Java) {
                "//"
            } else {
                "///"
            },
        ) {
            out.push(sym);
        }
    }
    out
}

fn extract_c_like_symbols(lang: &Lang, source: &str, file_path: &str) -> Vec<Symbol> {
    let lines: Vec<&str> = source.lines().collect();
    let include_re = Regex::new(r"^\s*#include\b.+").unwrap();
    let func_re =
        Regex::new(r"^\s*[A-Za-z_][A-Za-z0-9_\s\*]*\s+([A-Za-z_][A-Za-z0-9_]*)\s*\([^;]*\)\s*\{")
            .unwrap();
    let struct_re = Regex::new(r"^\s*struct\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let enum_re = Regex::new(r"^\s*enum\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let type_re = Regex::new(r"^\s*typedef\b.*\b([A-Za-z_][A-Za-z0-9_]*)\s*;").unwrap();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if include_re.is_match(trimmed) {
            out.push(simple_symbol(
                trimmed,
                SymbolKind::Import,
                file_path,
                i,
                i,
                Some(trimmed.to_string()),
                collect_line_comments(&lines, i, "//"),
            ));
            continue;
        }
        if let Some(sym) = capture_symbol(
            &lines,
            i,
            &[
                (&func_re, SymbolKind::Function),
                (&struct_re, SymbolKind::Struct),
                (&enum_re, SymbolKind::Enum),
                (&type_re, SymbolKind::Type),
            ],
            file_path,
            if matches!(lang, Lang::C) { "//" } else { "///" },
        ) {
            out.push(sym);
        }
    }
    out
}

fn extract_cpp_symbols(source: &str, file_path: &str) -> Vec<Symbol> {
    let mut out = extract_c_like_symbols(&Lang::Cpp, source, file_path);
    let lines: Vec<&str> = source.lines().collect();
    let class_re = Regex::new(r"^\s*(?:class|namespace)\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    for (i, line) in lines.iter().enumerate() {
        if let Some(cap) = class_re.captures(line.trim_start()) {
            let end = brace_block_end(&lines, i).unwrap_or(i);
            out.push(simple_symbol(
                &cap[1],
                SymbolKind::Class,
                file_path,
                i,
                end,
                Some(line.trim().to_string()),
                collect_line_comments(&lines, i, "//"),
            ));
        }
    }
    out
}

fn extract_ruby_symbols(source: &str, file_path: &str) -> Vec<Symbol> {
    let lines: Vec<&str> = source.lines().collect();
    let class_re = Regex::new(r"^\s*class\s+([A-Za-z_][A-Za-z0-9_:]*)").unwrap();
    let module_re = Regex::new(r"^\s*module\s+([A-Za-z_][A-Za-z0-9_:]*)").unwrap();
    let fn_re = Regex::new(r"^\s*def\s+(?:self\.)?([A-Za-z_][A-Za-z0-9_!?=]*)").unwrap();
    let mut out = Vec::new();
    for (i, _) in lines.iter().enumerate() {
        if let Some(sym) = capture_symbol(
            &lines,
            i,
            &[
                (&class_re, SymbolKind::Class),
                (&module_re, SymbolKind::Module),
                (&fn_re, SymbolKind::Function),
            ],
            file_path,
            "#",
        ) {
            let end = ruby_block_end(&lines, i).unwrap_or(sym.line_end as usize);
            out.push(Symbol {
                line_end: end as u32,
                ..sym
            });
        }
    }
    out
}

fn extract_bash_symbols(source: &str, file_path: &str) -> Vec<Symbol> {
    let lines: Vec<&str> = source.lines().collect();
    let fn_re =
        Regex::new(r"^\s*(?:function\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*(?:\(\))?\s*\{").unwrap();
    let var_re = Regex::new(r"^\s*([A-Za-z_][A-Za-z0-9_]*)=").unwrap();
    let mut out = Vec::new();
    for (i, _) in lines.iter().enumerate() {
        if let Some(sym) = capture_symbol(
            &lines,
            i,
            &[
                (&fn_re, SymbolKind::Function),
                (&var_re, SymbolKind::Variable),
            ],
            file_path,
            "#",
        ) {
            out.push(sym);
        }
    }
    out
}

fn capture_symbol(
    lines: &[&str],
    line_idx: usize,
    patterns: &[(&Regex, SymbolKind)],
    file_path: &str,
    comment_prefix: &str,
) -> Option<Symbol> {
    let trimmed = lines[line_idx].trim_start();
    for (regex, kind) in patterns {
        let cap = regex.captures(trimmed)?;
        let end = match kind {
            SymbolKind::Import | SymbolKind::Variable => line_idx,
            _ => brace_block_end(lines, line_idx).unwrap_or(line_idx),
        };
        return Some(simple_symbol(
            &cap[1],
            kind.clone(),
            file_path,
            line_idx,
            end,
            Some(trimmed.to_string()),
            collect_comments(lines, line_idx, comment_prefix),
        ));
    }
    None
}

fn simple_symbol(
    name: &str,
    kind: SymbolKind,
    file_path: &str,
    start: usize,
    end: usize,
    signature: Option<String>,
    doc_comment: Option<String>,
) -> Symbol {
    Symbol {
        name: name.to_string(),
        kind,
        file: file_path.to_string(),
        line_start: start as u32,
        line_end: end as u32,
        signature,
        doc_comment,
        children: Vec::new(),
    }
}

trait WithChildren {
    fn with_children(self, children: Vec<Symbol>) -> Symbol;
}

impl WithChildren for Symbol {
    fn with_children(mut self, children: Vec<Symbol>) -> Symbol {
        self.children = children;
        self
    }
}

fn collect_comments(lines: &[&str], line_idx: usize, prefix: &str) -> Option<String> {
    match prefix {
        "#" => collect_hash_comments(lines, line_idx),
        _ => collect_line_comments(lines, line_idx, prefix),
    }
}

fn collect_line_comments(lines: &[&str], line_idx: usize, prefix: &str) -> Option<String> {
    let mut out = Vec::new();
    let mut idx = line_idx;
    while idx > 0 {
        idx -= 1;
        let trimmed = lines[idx].trim();
        if trimmed.is_empty() {
            break;
        }
        if trimmed.starts_with(prefix) {
            out.push(trimmed.to_string());
            continue;
        }
        if trimmed.starts_with("#[") || trimmed.starts_with("@") {
            continue;
        }
        break;
    }
    if out.is_empty() {
        None
    } else {
        out.reverse();
        Some(out.join("\n"))
    }
}

fn collect_hash_comments(lines: &[&str], line_idx: usize) -> Option<String> {
    let mut out = Vec::new();
    let mut idx = line_idx;
    while idx > 0 {
        idx -= 1;
        let trimmed = lines[idx].trim();
        if trimmed.is_empty() {
            break;
        }
        if trimmed.starts_with('#') {
            out.push(trimmed.to_string());
            continue;
        }
        if trimmed.starts_with('@') {
            continue;
        }
        break;
    }
    if out.is_empty() {
        None
    } else {
        out.reverse();
        Some(out.join("\n"))
    }
}

fn brace_block_end(lines: &[&str], start: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut saw_open = false;
    for (idx, line) in lines.iter().enumerate().skip(start) {
        for ch in line.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    saw_open = true;
                }
                '}' if saw_open => {
                    depth -= 1;
                    if depth <= 0 {
                        return Some(idx);
                    }
                }
                _ => {}
            }
        }
        if idx > start && saw_open && depth <= 0 {
            return Some(idx);
        }
    }
    if saw_open {
        Some(lines.len().saturating_sub(1))
    } else {
        None
    }
}

fn python_block_end(lines: &[&str], start: usize, indent: usize) -> Option<usize> {
    let mut body_indent = None;
    for (idx, line) in lines.iter().enumerate().skip(start + 1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let current_indent = line.len().saturating_sub(line.trim_start().len());
        if body_indent.is_none() {
            body_indent = Some(current_indent);
        }
        if current_indent <= indent && !trimmed.starts_with('#') {
            return Some(idx.saturating_sub(1));
        }
    }
    Some(lines.len().saturating_sub(1))
}

fn ruby_block_end(lines: &[&str], start: usize) -> Option<usize> {
    let mut depth = 0i32;
    let opener =
        Regex::new(r"^\s*(class|module|def|if|unless|case|begin|for|while|until|do)\b").unwrap();
    let ender = Regex::new(r"^\s*end\b").unwrap();
    for (idx, line) in lines.iter().enumerate().skip(start) {
        let trimmed = line.trim_start();
        if opener.is_match(trimmed) {
            depth += 1;
        }
        if ender.is_match(trimmed) {
            depth -= 1;
            if depth <= 0 {
                return Some(idx);
            }
        }
    }
    Some(lines.len().saturating_sub(1))
}
