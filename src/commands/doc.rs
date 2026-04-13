use crate::doc_intel;
use crate::output::{CommandOutput, PlainOutput};
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

#[derive(serde::Serialize)]
struct OutlineHeading {
    level: u32,
    text: String,
    line: u32,
}

#[derive(serde::Serialize)]
struct OutlineOutput {
    headings: Vec<OutlineHeading>,
}

impl CommandOutput for OutlineOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.headings.is_empty() {
            writeln!(w, "No headings found.")?;
            return Ok(());
        }
        for h in &self.headings {
            let indent = "  ".repeat((h.level as usize).saturating_sub(1));
            let marker = "#".repeat(h.level as usize);
            writeln!(w, "{}{} {}  :{}", indent, marker, h.text, h.line)?;
        }
        Ok(())
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
    let output = OutlineOutput {
        headings: headings
            .into_iter()
            .map(|h| OutlineHeading {
                level: h.level as u32,
                text: h.text,
                line: h.line as u32,
            })
            .collect(),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

#[derive(serde::Serialize)]
struct SectionEntry {
    heading: String,
    file: String,
    line_start: u32,
    line_end: u32,
    body: String,
}

#[derive(serde::Serialize)]
struct SectionOutput {
    sections: Vec<SectionEntry>,
}

impl CommandOutput for SectionOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        let last = self.sections.len().saturating_sub(1);
        for (i, s) in self.sections.iter().enumerate() {
            writeln!(
                w,
                "── {} ({}) :{}–{} ──",
                s.heading, s.file, s.line_start, s.line_end
            )?;
            writeln!(w, "{}", s.body)?;
            if self.sections.len() > 1 && i != last {
                writeln!(w)?;
            }
        }
        Ok(())
    }
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
                let output = SectionOutput {
                    sections: vec![SectionEntry {
                        heading: s.heading.text,
                        file: path.display().to_string(),
                        line_start: s.heading.line as u32,
                        line_end: s.line_end as u32,
                        body: s.body,
                    }],
                };
                out!(ctx, "{}", crate::output::to_string(&output)?);
            }
            None => bail!("No section matching '{}' in {}", query, path.display()),
        }
    } else if path.is_dir() {
        let results = doc_intel::find_section_recursive(&path, query)?;
        if results.is_empty() {
            bail!("No section matching '{}' found.", query);
        }
        let output = SectionOutput {
            sections: results
                .into_iter()
                .map(|(file, s)| SectionEntry {
                    heading: s.heading.text,
                    file,
                    line_start: s.heading.line as u32,
                    line_end: s.line_end as u32,
                    body: s.body,
                })
                .collect(),
        };
        out!(ctx, "{}", crate::output::to_string(&output)?);
    } else {
        bail!("Path not found: {}", path.display());
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct SearchHit {
    file: String,
    line: u32,
    heading_level: u32,
    heading: String,
    context: String,
}

#[derive(serde::Serialize)]
struct SearchOutput {
    hits: Vec<SearchHit>,
}

impl CommandOutput for SearchOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "{} matches:", self.hits.len())?;
        for h in &self.hits {
            let prefix = "#".repeat(h.heading_level as usize);
            writeln!(
                w,
                "  {}:{} ({} {})  {}",
                h.file, h.line, prefix, h.heading, h.context
            )?;
        }
        Ok(())
    }
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
        out!(
            ctx,
            "{}",
            crate::output::to_string(&PlainOutput::new(format!("No matches for '{}'.", query)))?
        );
        return Ok(());
    }

    let output = SearchOutput {
        hits: hits
            .into_iter()
            .map(|h| SearchHit {
                file: h.file,
                line: h.line as u32,
                heading_level: h.heading_level as u32,
                heading: h.heading,
                context: h.context,
            })
            .collect(),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

#[derive(serde::Serialize)]
struct MapHeading {
    level: u32,
    text: String,
}

#[derive(serde::Serialize)]
struct MapFile {
    file: String,
    headings: Vec<MapHeading>,
}

#[derive(serde::Serialize)]
struct MapOutput {
    files: Vec<MapFile>,
}

impl CommandOutput for MapOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.files.is_empty() {
            writeln!(w, "No markdown files found.")?;
            return Ok(());
        }
        for map in &self.files {
            writeln!(w, "{}", map.file)?;
            for h in &map.headings {
                let indent = "  ".repeat(h.level as usize);
                writeln!(w, "{}{}", indent, h.text)?;
            }
        }
        Ok(())
    }
}

fn cmd_doc_map(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let path_arg = args.iter().find(|a| !a.starts_with('-'));
    let path = match path_arg {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    };

    let files = if path.is_file() {
        let map = doc_intel::map_file(&path)?;
        vec![convert_file_map(&map)]
    } else if path.is_dir() {
        doc_intel::map_directory(&path)?
            .iter()
            .map(convert_file_map)
            .collect()
    } else {
        bail!("Path not found: {}", path.display());
    };

    let output = MapOutput { files };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

fn convert_file_map(map: &doc_intel::FileMap) -> MapFile {
    MapFile {
        file: map.file.clone(),
        headings: map
            .headings
            .iter()
            .map(|h| MapHeading {
                level: h.level as u32,
                text: h.text.clone(),
            })
            .collect(),
    }
}
