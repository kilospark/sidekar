//! Code intelligence via tree-sitter.
//!
//! Provides structured code navigation (symbols, definitions, references,
//! structure) without requiring an LSP server. Queries donated from Rhizome
//! (github.com/basidiocarp/rhizome, MIT).

use std::fmt;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Result, anyhow, bail};

// ── Language detection ───────────────────────────────────────────────────────

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
            "ts" | "mts" | "cts" => Some(Lang::TypeScript),
            "tsx" => Some(Lang::TypeScript), // handled specially at parse time
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

    fn ts_language(&self, is_tsx: bool) -> tree_sitter::Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::TypeScript if is_tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            Lang::Java => tree_sitter_java::LANGUAGE.into(),
            Lang::C => tree_sitter_c::LANGUAGE.into(),
            Lang::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Lang::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            Lang::Bash => tree_sitter_bash::LANGUAGE.into(),
            Lang::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
        }
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

// ── Symbol types ─────────────────────────────────────────────────────────────

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

// ── Tree-sitter queries (donated from Rhizome) ──────────────────────────────

const RUST_QUERY: &str = r#"
(function_item name: (identifier) @name) @function
(struct_item name: (type_identifier) @name) @struct_def
(enum_item name: (type_identifier) @name) @enum_def
(trait_item name: (type_identifier) @name) @trait_def
(impl_item type: [(type_identifier) (generic_type)] @name) @impl_def
(use_declaration) @import
(const_item name: (identifier) @name) @const_def
(static_item name: (identifier) @name) @static_def
(mod_item name: (identifier) @name) @mod_def
(macro_definition name: (identifier) @name) @function
"#;

const PYTHON_QUERY: &str = r#"
(function_definition name: (identifier) @name) @function
(class_definition name: (identifier) @name) @class_def
(import_statement) @import
(import_from_statement) @import
"#;

const JAVASCRIPT_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @function
(class_declaration name: (identifier) @name) @class_def
(import_statement) @import
(lexical_declaration) @variable
"#;

const TYPESCRIPT_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @function
(class_declaration name: (type_identifier) @name) @class_def
(interface_declaration name: (type_identifier) @name) @trait_def
(type_alias_declaration name: (type_identifier) @name) @type_def
(enum_declaration name: (identifier) @name) @enum_def
(import_statement) @import
(lexical_declaration) @variable
"#;

const GO_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @function
(method_declaration name: (field_identifier) @name) @function
(type_declaration (type_spec name: (type_identifier) @name)) @type_def
(import_declaration) @import
"#;

const JAVA_QUERY: &str = r#"
(class_declaration name: (identifier) @name) @class_def
(interface_declaration name: (identifier) @name) @trait_def
(method_declaration name: (identifier) @name) @function
(constructor_declaration name: (identifier) @name) @function
(enum_declaration name: (identifier) @name) @enum_def
(import_declaration) @import
"#;

const C_QUERY: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @name)) @function
(struct_specifier name: (type_identifier) @name) @struct_def
(enum_specifier name: (type_identifier) @name) @enum_def
(type_definition declarator: (type_identifier) @name) @type_def
(declaration declarator: (function_declarator declarator: (identifier) @name)) @function
"#;

const CPP_QUERY: &str = r#"
(class_specifier name: (type_identifier) @name) @class_def
(struct_specifier name: (type_identifier) @name) @struct_def
(enum_specifier name: (type_identifier) @name) @enum_def
(namespace_definition name: (namespace_identifier) @name) @type_def
(function_definition declarator: (function_declarator declarator: (identifier) @name)) @function
(function_definition declarator: (function_declarator declarator: (qualified_identifier name: (identifier) @name))) @function
"#;

const RUBY_QUERY: &str = r#"
(class name: (constant) @name) @class_def
(module name: (constant) @name) @type_def
(method name: (identifier) @name) @function
(singleton_method name: (identifier) @name) @function
"#;

const BASH_QUERY: &str = r#"
(function_definition name: (word) @name) @function
(variable_assignment name: (variable_name) @name) @variable
"#;

const CSHARP_QUERY: &str = r#"
(class_declaration name: (identifier) @name) @class_def
(method_declaration name: (identifier) @name) @function
(interface_declaration name: (identifier) @name) @trait_def
(struct_declaration name: (identifier) @name) @struct_def
(enum_declaration name: (identifier) @name) @enum_def
(using_directive) @import
"#;

// ── Compiled query cache ─────────────────────────────────────────────────────

macro_rules! query_cache {
    ($($name:ident),+ $(,)?) => {
        $(static $name: OnceLock<Result<tree_sitter::Query, String>> = OnceLock::new();)+
    };
}

query_cache!(
    Q_RUST, Q_PYTHON, Q_JS, Q_TS, Q_TSX, Q_GO, Q_JAVA,
    Q_C, Q_CPP, Q_RUBY, Q_BASH, Q_CSHARP,
);

fn compiled_query(lang: &Lang, is_tsx: bool) -> Result<&'static tree_sitter::Query> {
    let ts_lang = lang.ts_language(is_tsx);
    let (cache, source) = match (lang, is_tsx) {
        (Lang::Rust, _) => (&Q_RUST, RUST_QUERY),
        (Lang::Python, _) => (&Q_PYTHON, PYTHON_QUERY),
        (Lang::JavaScript, _) => (&Q_JS, JAVASCRIPT_QUERY),
        (Lang::TypeScript, false) => (&Q_TS, TYPESCRIPT_QUERY),
        (Lang::TypeScript, true) => (&Q_TSX, TYPESCRIPT_QUERY),
        (Lang::Go, _) => (&Q_GO, GO_QUERY),
        (Lang::Java, _) => (&Q_JAVA, JAVA_QUERY),
        (Lang::C, _) => (&Q_C, C_QUERY),
        (Lang::Cpp, _) => (&Q_CPP, CPP_QUERY),
        (Lang::Ruby, _) => (&Q_RUBY, RUBY_QUERY),
        (Lang::Bash, _) => (&Q_BASH, BASH_QUERY),
        (Lang::CSharp, _) => (&Q_CSHARP, CSHARP_QUERY),
    };

    let result = cache.get_or_init(|| {
        tree_sitter::Query::new(&ts_lang, source).map_err(|e| format!("{e}"))
    });

    result
        .as_ref()
        .map_err(|e| anyhow!("query compile error: {e}"))
}

// ── Parsing ──────────────────────────────────────────────────────────────────

fn parse_source(lang: &Lang, source: &[u8], is_tsx: bool) -> Result<tree_sitter::Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang.ts_language(is_tsx))?;
    parser.parse(source, None).ok_or_else(|| anyhow!("parse failed"))
}

// ── Symbol extraction ────────────────────────────────────────────────────────

fn capture_kind_to_symbol_kind(kind: &str) -> SymbolKind {
    match kind {
        "function" => SymbolKind::Function,
        "struct_def" => SymbolKind::Struct,
        "enum_def" => SymbolKind::Enum,
        "trait_def" => SymbolKind::Trait,
        "impl_def" => SymbolKind::Struct,
        "class_def" => SymbolKind::Class,
        "type_def" => SymbolKind::Type,
        "mod_def" => SymbolKind::Module,
        "const_def" | "static_def" => SymbolKind::Constant,
        "variable" => SymbolKind::Variable,
        "import" => SymbolKind::Import,
        _ => SymbolKind::Variable,
    }
}

fn extract_signature(node: tree_sitter::Node, source: &[u8], lang: &Lang) -> Option<String> {
    let text = node.utf8_text(source).ok()?;
    let delim = match lang {
        Lang::Python => ':',
        _ => '{',
    };
    let sig = if let Some(pos) = text.find(delim) {
        text[..pos].trim()
    } else {
        text.lines().next().unwrap_or(text).trim()
    };
    if sig.is_empty() { None } else { Some(sig.to_string()) }
}

fn extract_doc_comment(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut lines = Vec::new();
    let mut sib = node.prev_named_sibling();
    while let Some(s) = sib {
        match s.kind() {
            "line_comment" | "comment" | "block_comment" => {
                if let Ok(t) = s.utf8_text(source) {
                    lines.push(t.to_string());
                }
                sib = s.prev_named_sibling();
            }
            "attribute_item" | "decorator" => {
                sib = s.prev_named_sibling();
            }
            _ => break,
        }
    }
    if lines.is_empty() {
        // Python docstrings
        if let Some(body) = node.child_by_field_name("body") {
            if let Some(first) = body.named_child(0) {
                if first.kind() == "expression_statement" {
                    if let Some(s) = first.named_child(0) {
                        if s.kind() == "string" {
                            return s.utf8_text(source).ok().map(String::from);
                        }
                    }
                }
            }
        }
        return None;
    }
    lines.reverse();
    Some(lines.join("\n"))
}

/// Extract impl block methods (Rust-specific).
fn extract_impl_children(
    node: tree_sitter::Node,
    source: &[u8],
    file_path: &str,
    parent: &str,
) -> Vec<Symbol> {
    let mut methods = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "declaration_list" {
            let mut inner = child.walk();
            for item in child.named_children(&mut inner) {
                if item.kind() == "function_item" {
                    if let Some(name_node) = item.child_by_field_name("name") {
                        let name = name_node.utf8_text(source).unwrap_or_default().to_string();
                        methods.push(Symbol {
                            name,
                            kind: SymbolKind::Method,
                            file: file_path.to_string(),
                            line_start: item.start_position().row as u32,
                            line_end: item.end_position().row as u32,
                            signature: extract_signature(item, source, &Lang::Rust),
                            doc_comment: extract_doc_comment(item, source),
                            children: Vec::new(),
                        });
                    }
                }
            }
        }
    }
    let _ = parent; // used for scope in future
    methods
}

/// Extract all symbols from a single file.
pub fn extract_symbols(path: &Path) -> Result<Vec<Symbol>> {
    let lang = Lang::from_path(path).ok_or_else(|| anyhow!("unsupported file type"))?;
    let source = std::fs::read(path)?;
    let file_path = path.to_string_lossy().to_string();
    let is_tsx = path.extension().and_then(|e| e.to_str()) == Some("tsx");

    let tree = parse_source(&lang, &source, is_tsx)?;
    let query = compiled_query(&lang, is_tsx)?;
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_slice());

    let capture_names = query.capture_names();
    let mut symbols = Vec::new();

    use tree_sitter::StreamingIterator;
    while let Some(m) = matches.next() {
        let mut name_text = String::new();
        let mut def_node: Option<tree_sitter::Node> = None;
        let mut capture_kind = "";

        for cap in m.captures {
            let cap_name = capture_names[cap.index as usize];
            if cap_name == "name" {
                name_text = cap.node.utf8_text(&source).unwrap_or_default().to_string();
            } else {
                capture_kind = cap_name;
                def_node = Some(cap.node);
            }
        }

        let Some(node) = def_node else { continue };

        // Imports don't have @name capture
        if capture_kind == "import" && name_text.is_empty() {
            let text = node.utf8_text(&source).unwrap_or_default();
            name_text = text.lines().next().unwrap_or(text).trim().to_string();
            if name_text.len() > 200 {
                name_text.truncate(200);
            }
        }

        if name_text.is_empty() {
            continue;
        }

        let kind = capture_kind_to_symbol_kind(capture_kind);

        let children = if capture_kind == "impl_def" {
            extract_impl_children(node, &source, &file_path, &name_text)
        } else {
            Vec::new()
        };

        // Skip standalone function_item that's inside an impl block (parent is
        // declaration_list inside impl_item). These are captured as impl children.
        if capture_kind == "function" {
            let mut p = node.parent();
            while let Some(parent) = p {
                if parent.kind() == "impl_item" {
                    break;
                }
                p = parent.parent();
            }
            if p.is_some() {
                continue;
            }
        }

        symbols.push(Symbol {
            name: name_text,
            kind,
            file: file_path.clone(),
            line_start: node.start_position().row as u32,
            line_end: node.end_position().row as u32,
            signature: extract_signature(node, &source, &lang),
            doc_comment: extract_doc_comment(node, &source),
            children,
        });
    }

    Ok(symbols)
}

// ── Multi-file operations ────────────────────────────────────────────────────

/// Walk a directory and extract symbols from all supported files.
pub fn extract_symbols_recursive(root: &Path) -> Result<Vec<Symbol>> {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true) // skip dotdirs
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
            Err(_) => continue, // skip unparseable files
        }
    }
    Ok(all)
}

/// Find the definition of a symbol by name across a directory.
pub fn find_definition(root: &Path, name: &str) -> Result<Vec<Symbol>> {
    let all = extract_symbols_recursive(root)?;
    let matches: Vec<Symbol> = all
        .into_iter()
        .filter(|s| {
            s.kind != SymbolKind::Import
                && s.kind != SymbolKind::Variable
                && (s.name == name
                    || s.children.iter().any(|c| c.name == name))
        })
        .flat_map(|s| {
            // If a child matches, return the child instead
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

/// Find references to a symbol name across a directory (text-based, not scope-aware).
pub fn find_references(root: &Path, name: &str) -> Result<Vec<Reference>> {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .build();

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
        let lang = match Lang::from_path(path) {
            Some(l) => l,
            None => continue,
        };

        let source = match std::fs::read(path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let is_tsx = path.extension().and_then(|e| e.to_str()) == Some("tsx");
        let tree = match parse_source(&lang, &source, is_tsx) {
            Ok(t) => t,
            Err(_) => continue,
        };

        find_identifier_nodes(
            tree.root_node(),
            &source,
            name,
            &path.to_string_lossy(),
            &mut refs,
        );
    }
    Ok(refs)
}

#[derive(Debug, Clone)]
pub struct Reference {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub context: String, // the line of code
}

fn find_identifier_nodes(
    node: tree_sitter::Node,
    source: &[u8],
    name: &str,
    file_path: &str,
    refs: &mut Vec<Reference>,
) {
    let kind = node.kind();
    if kind == "identifier" || kind == "type_identifier" || kind == "field_identifier" {
        if let Ok(text) = node.utf8_text(source) {
            if text == name {
                let line = node.start_position().row as u32;
                let col = node.start_position().column as u32;
                // Extract the full source line for context
                let line_text = std::str::from_utf8(source)
                    .ok()
                    .and_then(|s| s.lines().nth(line as usize))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                refs.push(Reference {
                    file: file_path.to_string(),
                    line,
                    column: col,
                    context: line_text,
                });
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_identifier_nodes(child, source, name, file_path, refs);
    }
}

/// Get the full source text of a symbol (definition body).
pub fn get_symbol_body(path: &Path, name: &str) -> Result<String> {
    let symbols = extract_symbols(path)?;
    // Check top-level symbols and their children
    for sym in &symbols {
        if sym.name == name && sym.kind != SymbolKind::Import {
            let source = std::fs::read_to_string(path)?;
            let lines: Vec<&str> = source.lines().collect();
            let start = sym.line_start as usize;
            let end = (sym.line_end as usize + 1).min(lines.len());
            return Ok(lines[start..end].join("\n"));
        }
        for child in &sym.children {
            if child.name == name {
                let source = std::fs::read_to_string(path)?;
                let lines: Vec<&str> = source.lines().collect();
                let start = child.line_start as usize;
                let end = (child.line_end as usize + 1).min(lines.len());
                return Ok(lines[start..end].join("\n"));
            }
        }
    }
    bail!("symbol '{name}' not found in {}", path.display())
}

// ── Formatting helpers ───────────────────────────────────────────────────────

/// Format symbol list for compact CLI output (one line per symbol).
pub fn format_symbols(symbols: &[Symbol], root: &Path) -> String {
    let mut out = String::new();
    for s in symbols {
        if s.kind == SymbolKind::Import {
            continue; // skip imports in default output
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

/// Format references for compact CLI output.
pub fn format_references(refs: &[Reference], root: &Path) -> String {
    let mut out = String::new();
    for r in refs {
        let rel = pathdiff(root, &r.file);
        out.push_str(&format!("{}:{}:{} {}\n", rel, r.line + 1, r.column + 1, r.context));
    }
    out
}

fn pathdiff(root: &Path, file: &str) -> String {
    let fp = Path::new(file);
    fp.strip_prefix(root)
        .unwrap_or(fp)
        .to_string_lossy()
        .to_string()
}
