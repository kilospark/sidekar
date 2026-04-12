use super::*;

pub(crate) fn extract_symbols_from_source(
    lang: &Lang,
    source: &str,
    file_path: &str,
) -> Vec<Symbol> {
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
