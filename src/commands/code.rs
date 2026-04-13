use crate::code_intel;
use crate::*;
use std::path::Path;

#[derive(serde::Serialize)]
struct SymbolOut {
    name: String,
    kind: code_intel::SymbolKind,
    rel_path: String,
    line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
    children: Vec<SymbolOut>,
}

#[derive(serde::Serialize)]
struct SymbolsOutput {
    items: Vec<SymbolOut>,
}

impl crate::output::CommandOutput for SymbolsOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "No symbols found.")?;
            return Ok(());
        }
        for s in &self.items {
            if let Some(sig) = &s.signature {
                writeln!(w, "{:<8} {}  {}:{}", s.kind, sig, s.rel_path, s.line)?;
            } else {
                writeln!(w, "{:<8} {:<30} {}:{}", s.kind, s.name, s.rel_path, s.line)?;
            }
            for child in &s.children {
                if let Some(sig) = &child.signature {
                    writeln!(
                        w,
                        "  {:<6} {}  {}:{}",
                        child.kind, sig, child.rel_path, child.line
                    )?;
                } else {
                    writeln!(
                        w,
                        "  {:<6} {:<28} {}:{}",
                        child.kind, child.name, child.rel_path, child.line
                    )?;
                }
            }
        }
        Ok(())
    }
}

fn symbol_to_out(sym: &code_intel::Symbol, root: &Path) -> SymbolOut {
    SymbolOut {
        name: sym.name.clone(),
        kind: sym.kind.clone(),
        rel_path: rel_path(root, &sym.file),
        line: sym.line_start + 1,
        signature: sym.signature.clone(),
        children: sym
            .children
            .iter()
            .map(|c| symbol_to_out(c, root))
            .collect(),
    }
}

pub fn cmd_symbols(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let show_imports = args.iter().any(|a| a == "--imports");
    let path_arg = args.iter().find(|a| !a.starts_with('-'));

    let path = match path_arg {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    };

    if !path.exists() {
        bail!("path not found: {}", path.display());
    }

    let symbols = if path.is_dir() {
        code_intel::extract_symbols_recursive(&path)?
    } else {
        code_intel::extract_symbols(&path)?
    };

    let root = if path.is_dir() {
        path.clone()
    } else {
        path.parent().unwrap_or(Path::new(".")).to_path_buf()
    };

    let items: Vec<SymbolOut> = symbols
        .iter()
        .filter(|s| show_imports || s.kind != code_intel::SymbolKind::Import)
        .map(|s| symbol_to_out(s, &root))
        .collect();

    let output = SymbolsOutput { items };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

#[derive(serde::Serialize)]
struct DefinitionMatchOut {
    rel_path: String,
    line: u32,
    doc_comment: Option<String>,
    body: String,
    start_line: u32,
}

#[derive(serde::Serialize)]
struct DefinitionOutput {
    name: String,
    matches: Vec<DefinitionMatchOut>,
}

impl crate::output::CommandOutput for DefinitionOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        for (i, m) in self.matches.iter().enumerate() {
            if m.line == 0 {
                writeln!(w, "── {} ──", m.rel_path)?;
                writeln!(w, "{}", m.body)?;
            } else {
                writeln!(w, "── {}:{} ──", m.rel_path, m.line)?;
                if let Some(doc) = &m.doc_comment {
                    writeln!(w, "{doc}")?;
                }
                for (j, line) in m.body.lines().enumerate() {
                    writeln!(w, "{:>4} {}", m.start_line as usize + j, line)?;
                }
                writeln!(w)?;
            }
            let _ = i;
        }
        Ok(())
    }
}

pub fn cmd_definition(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let name = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| anyhow!("Usage: sidekar definition <name> [path]"))?;

    let path_arg = args.iter().filter(|a| !a.starts_with('-')).nth(1);
    let root = match path_arg {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    };

    if root.is_file() {
        let body = code_intel::get_symbol_body(&root, name)?;
        let rel = rel_path(
            root.parent().unwrap_or(Path::new(".")),
            &root.to_string_lossy(),
        );
        let output = DefinitionOutput {
            name: name.to_string(),
            matches: vec![DefinitionMatchOut {
                rel_path: rel,
                line: 0,
                doc_comment: None,
                body,
                start_line: 0,
            }],
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
        return Ok(());
    }

    let matches = code_intel::find_definition(&root, name)?;
    if matches.is_empty() {
        bail!("no definition found for '{name}'");
    }

    let mut match_outs: Vec<DefinitionMatchOut> = Vec::new();
    for sym in &matches {
        let rel = rel_path(&root, &sym.file);
        let file_path = Path::new(&sym.file);
        let body = if let Ok(source) = std::fs::read_to_string(file_path) {
            let lines: Vec<&str> = source.lines().collect();
            let start = sym.line_start as usize;
            let end = (sym.line_end as usize + 1).min(lines.len());
            lines[start..end].join("\n")
        } else {
            String::new()
        };
        match_outs.push(DefinitionMatchOut {
            rel_path: rel,
            line: sym.line_start + 1,
            doc_comment: sym.doc_comment.clone(),
            body,
            start_line: sym.line_start + 1,
        });
    }
    let output = DefinitionOutput {
        name: name.to_string(),
        matches: match_outs,
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

#[derive(serde::Serialize)]
struct ReferenceOut {
    rel_path: String,
    line: u32,
    column: u32,
    context: String,
}

#[derive(serde::Serialize)]
struct ReferencesOutput {
    name: String,
    items: Vec<ReferenceOut>,
}

impl crate::output::CommandOutput for ReferencesOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "No references found for '{}'.", self.name)?;
            return Ok(());
        }
        writeln!(w, "{} references to '{}':", self.items.len(), self.name)?;
        for r in &self.items {
            writeln!(w, "  {}:{}:{} {}", r.rel_path, r.line, r.column, r.context)?;
        }
        Ok(())
    }
}

pub fn cmd_references(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let name = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| anyhow!("Usage: sidekar references <name> [path]"))?;

    let path_arg = args.iter().filter(|a| !a.starts_with('-')).nth(1);
    let root = match path_arg {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    };

    let refs = code_intel::find_references(&root, name)?;
    let items: Vec<ReferenceOut> = refs
        .into_iter()
        .map(|r| ReferenceOut {
            rel_path: rel_path(&root, &r.file),
            line: r.line + 1,
            column: r.column + 1,
            context: r.context,
        })
        .collect();

    let output = ReferencesOutput {
        name: name.clone(),
        items,
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

#[derive(serde::Serialize)]
struct StructureChildOut {
    kind: code_intel::SymbolKind,
    name: String,
}

#[derive(serde::Serialize)]
struct StructureSymbolOut {
    kind: code_intel::SymbolKind,
    name: String,
    children: Vec<StructureChildOut>,
}

#[derive(serde::Serialize)]
struct StructureFileOut {
    file: String,
    symbols: Vec<StructureSymbolOut>,
}

#[derive(serde::Serialize)]
struct StructureOutput {
    files: Vec<StructureFileOut>,
}

impl crate::output::CommandOutput for StructureOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.files.is_empty() {
            writeln!(w, "No symbols found.")?;
            return Ok(());
        }
        for file in &self.files {
            writeln!(w, "{}", file.file)?;
            for s in &file.symbols {
                writeln!(w, "  {} {}", s.kind, s.name)?;
                for child in &s.children {
                    writeln!(w, "    {} {}", child.kind, child.name)?;
                }
            }
        }
        Ok(())
    }
}

pub fn cmd_structure(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let path_arg = args.iter().find(|a| !a.starts_with('-'));
    let path = match path_arg {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    };

    if !path.exists() {
        bail!("path not found: {}", path.display());
    }

    let root = if path.is_dir() {
        path.clone()
    } else {
        path.parent().unwrap_or(Path::new(".")).to_path_buf()
    };

    let symbols = if path.is_dir() {
        code_intel::extract_symbols_recursive(&path)?
    } else {
        code_intel::extract_symbols(&path)?
    };

    let mut by_file: std::collections::BTreeMap<String, Vec<&code_intel::Symbol>> =
        std::collections::BTreeMap::new();
    for s in &symbols {
        if s.kind == code_intel::SymbolKind::Import {
            continue;
        }
        let rel = rel_path(&root, &s.file);
        by_file.entry(rel).or_default().push(s);
    }

    let files: Vec<StructureFileOut> = by_file
        .into_iter()
        .map(|(file, syms)| StructureFileOut {
            file,
            symbols: syms
                .into_iter()
                .map(|s| StructureSymbolOut {
                    kind: s.kind.clone(),
                    name: s.name.clone(),
                    children: s
                        .children
                        .iter()
                        .map(|c| StructureChildOut {
                            kind: c.kind.clone(),
                            name: c.name.clone(),
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();

    let output = StructureOutput { files };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

fn rel_path(root: &Path, file: &str) -> String {
    let fp = Path::new(file);
    fp.strip_prefix(root)
        .unwrap_or(fp)
        .to_string_lossy()
        .to_string()
}
