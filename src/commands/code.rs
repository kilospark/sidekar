use crate::code_intel;
use crate::*;
use std::path::Path;

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

    if symbols.is_empty() {
        out!(ctx, "No symbols found.");
        return Ok(());
    }

    let root = if path.is_dir() {
        path.clone()
    } else {
        path.parent().unwrap_or(Path::new(".")).to_path_buf()
    };

    for s in &symbols {
        if !show_imports && s.kind == code_intel::SymbolKind::Import {
            continue;
        }
        let rel = rel_path(&root, &s.file);
        if let Some(sig) = &s.signature {
            out!(ctx, "{:<8} {}  {}:{}", s.kind, sig, rel, s.line_start + 1);
        } else {
            out!(
                ctx,
                "{:<8} {:<30} {}:{}",
                s.kind,
                s.name,
                rel,
                s.line_start + 1
            );
        }
        for child in &s.children {
            if let Some(sig) = &child.signature {
                out!(ctx, "  {:<6} {}  {}:{}", child.kind, sig, rel, child.line_start + 1);
            } else {
                out!(
                    ctx,
                    "  {:<6} {:<28} {}:{}",
                    child.kind,
                    child.name,
                    rel,
                    child.line_start + 1
                );
            }
        }
    }
    Ok(())
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

    // If root is a file, try to get the symbol body directly
    if root.is_file() {
        let body = code_intel::get_symbol_body(&root, name)?;
        let rel = rel_path(
            root.parent().unwrap_or(Path::new(".")),
            &root.to_string_lossy(),
        );
        out!(ctx, "── {} ──", rel);
        out!(ctx, "{body}");
        return Ok(());
    }

    let matches = code_intel::find_definition(&root, name)?;
    if matches.is_empty() {
        bail!("no definition found for '{name}'");
    }

    for sym in &matches {
        let rel = rel_path(&root, &sym.file);
        let file_path = Path::new(&sym.file);
        out!(ctx, "── {}:{} ──", rel, sym.line_start + 1);

        if let Some(doc) = &sym.doc_comment {
            out!(ctx, "{doc}");
        }

        // Read and output the source lines
        if let Ok(source) = std::fs::read_to_string(file_path) {
            let lines: Vec<&str> = source.lines().collect();
            let start = sym.line_start as usize;
            let end = (sym.line_end as usize + 1).min(lines.len());
            for (i, line) in lines[start..end].iter().enumerate() {
                out!(ctx, "{:>4} {}", start + i + 1, line);
            }
        }
        out!(ctx, "");
    }
    Ok(())
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
    if refs.is_empty() {
        out!(ctx, "No references found for '{name}'.");
        return Ok(());
    }

    out!(ctx, "{} references to '{name}':", refs.len());
    for r in &refs {
        let rel = rel_path(&root, &r.file);
        out!(ctx, "  {}:{}:{} {}", rel, r.line + 1, r.column + 1, r.context);
    }
    Ok(())
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

    if symbols.is_empty() {
        out!(ctx, "No symbols found.");
        return Ok(());
    }

    // Group by file
    let mut by_file: std::collections::BTreeMap<String, Vec<&code_intel::Symbol>> =
        std::collections::BTreeMap::new();
    for s in &symbols {
        if s.kind == code_intel::SymbolKind::Import {
            continue;
        }
        let rel = rel_path(&root, &s.file);
        by_file.entry(rel).or_default().push(s);
    }

    for (file, syms) in &by_file {
        out!(ctx, "{file}");
        for s in syms {
            out!(ctx, "  {} {}", s.kind, s.name);
            for child in &s.children {
                out!(ctx, "    {} {}", child.kind, child.name);
            }
        }
    }
    Ok(())
}

fn rel_path(root: &Path, file: &str) -> String {
    let fp = Path::new(file);
    fp.strip_prefix(root)
        .unwrap_or(fp)
        .to_string_lossy()
        .to_string()
}
