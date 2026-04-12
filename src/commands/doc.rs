use crate::doc_intel;
use crate::*;

pub fn cmd_doc(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar doc <outline|section|search|map> [args]");
    }
    match args[0].as_str() {
        "outline" => cmd_doc_outline(ctx, &args[1..]),
        "section" => cmd_doc_section(ctx, &args[1..]),
        "search" => cmd_doc_search(ctx, &args[1..]),
        "map" => cmd_doc_map(ctx, &args[1..]),
        _ => bail!(
            "Unknown subcommand: {}. Use: outline, section, search, map",
            args[0]
        ),
    }
}

fn cmd_doc_outline(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let path_arg = args.iter().find(|a| !a.starts_with('-'));
    let path = match path_arg {
        Some(p) => std::path::PathBuf::from(p),
        None => bail!("Usage: sidekar doc outline <file.md>"),
    };

    if !path.exists() {
        bail!("File not found: {}", path.display());
    }

    let headings = doc_intel::extract_outline(&path)?;
    if headings.is_empty() {
        out!(ctx, "No headings found.");
        return Ok(());
    }

    for h in &headings {
        let indent = "  ".repeat((h.level as usize).saturating_sub(1));
        let marker = "#".repeat(h.level as usize);
        out!(ctx, "{}{} {}  :{}", indent, marker, h.text, h.line);
    }
    Ok(())
}

fn cmd_doc_section(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar doc section <heading> [path]");
    }

    let query = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| anyhow!("Usage: sidekar doc section <heading> [path]"))?;

    let path_arg = args.iter().filter(|a| !a.starts_with('-')).nth(1);
    let path = match path_arg {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    };

    if path.is_file() {
        let section = doc_intel::find_section(&path, query)?;
        match section {
            Some(s) => {
                out!(
                    ctx,
                    "── {} ({}) :{}–{} ──",
                    s.heading.text,
                    path.display(),
                    s.heading.line,
                    s.line_end
                );
                out!(ctx, "{}", s.body);
            }
            None => bail!("No section matching '{}' in {}", query, path.display()),
        }
    } else if path.is_dir() {
        let results = doc_intel::find_section_recursive(&path, query)?;
        if results.is_empty() {
            bail!("No section matching '{}' found.", query);
        }
        for (file, s) in &results {
            out!(
                ctx,
                "── {} ({}) :{}–{} ──",
                s.heading.text,
                file,
                s.heading.line,
                s.line_end
            );
            out!(ctx, "{}", s.body);
            out!(ctx, "");
        }
    } else {
        bail!("Path not found: {}", path.display());
    }
    Ok(())
}

fn cmd_doc_search(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar doc search <query> [path]");
    }

    let query = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| anyhow!("Usage: sidekar doc search <query> [path]"))?;

    let path_arg = args.iter().filter(|a| !a.starts_with('-')).nth(1);
    let path = match path_arg {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    };

    let hits = if path.is_file() {
        doc_intel::search_file(&path, query)?
    } else if path.is_dir() {
        doc_intel::search_recursive(&path, query)?
    } else {
        bail!("Path not found: {}", path.display());
    };

    if hits.is_empty() {
        out!(ctx, "No matches for '{}'.", query);
        return Ok(());
    }

    out!(ctx, "{} matches:", hits.len());
    for h in &hits {
        let prefix = "#".repeat(h.heading_level as usize);
        out!(
            ctx,
            "  {}:{} ({} {})  {}",
            h.file,
            h.line,
            prefix,
            h.heading,
            h.context
        );
    }
    Ok(())
}

fn cmd_doc_map(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let path_arg = args.iter().find(|a| !a.starts_with('-'));
    let path = match path_arg {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    };

    if path.is_file() {
        let map = doc_intel::map_file(&path)?;
        print_file_map(ctx, &map);
    } else if path.is_dir() {
        let maps = doc_intel::map_directory(&path)?;
        if maps.is_empty() {
            out!(ctx, "No markdown files found.");
            return Ok(());
        }
        for map in &maps {
            print_file_map(ctx, map);
        }
    } else {
        bail!("Path not found: {}", path.display());
    }
    Ok(())
}

fn print_file_map(ctx: &mut AppContext, map: &doc_intel::FileMap) {
    out!(ctx, "{}", map.file);
    for h in &map.headings {
        let indent = "  ".repeat(h.level as usize);
        out!(ctx, "{}{}", indent, h.text);
    }
}
