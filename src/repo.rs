use crate::*;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

const DEFAULT_MAX_FILE_BYTES: usize = 1_000_000;
const DEFAULT_IGNORES: &[&str] = &[
    ".git/**",
    "**/.git/**",
    "node_modules/**",
    "**/node_modules/**",
    "target/**",
    "**/target/**",
    "dist/**",
    "**/dist/**",
    "build/**",
    "**/build/**",
    ".next/**",
    "**/.next/**",
    ".turbo/**",
    "**/.turbo/**",
    ".cache/**",
    "**/.cache/**",
    "coverage/**",
    "**/coverage/**",
];

mod args;
mod pack;
mod render;

use args::*;
use pack::*;
use render::*;

#[derive(Debug)]
struct RepoArgs {
    target: Option<String>,
    include: Vec<String>,
    ignore: Vec<String>,
    stdin: bool,
    max_file_bytes: usize,
}

#[derive(Clone, Debug, Serialize)]
struct RepoFile {
    path: String,
    chars: usize,
    est_tokens: usize,
    language: Option<&'static str>,
    content: String,
}

#[derive(Clone, Debug, Serialize)]
struct SkippedFile {
    path: String,
    reason: String,
}

#[derive(Debug, Serialize)]
struct RepoSnapshot {
    root: PathBuf,
    display_root: String,
    total_chars: usize,
    total_est_tokens: usize,
    files: Vec<RepoFile>,
    skipped: Vec<SkippedFile>,
    tree: String,
}

impl crate::output::CommandOutput for RepoSnapshot {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        write!(w, "{}", render_markdown(self))
    }
}

#[derive(Default)]
struct TreeNode {
    dirs: BTreeMap<String, TreeNode>,
    files: BTreeMap<String, usize>,
    file_count: usize,
    est_tokens: usize,
}

pub fn cmd_repo(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "pack" => cmd_repo_pack(ctx, &args[1..]),
        "tree" => cmd_repo_tree(ctx, &args[1..]),
        "" => bail!("Usage: sidekar repo <pack|tree> ..."),
        other => bail!("Unknown repo subcommand: {other}"),
    }
}

fn cmd_repo_pack(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let opts = parse_repo_args(args)?;
    let snapshot = build_repo_snapshot(&opts)?;
    out!(ctx, "{}", crate::output::to_string(&snapshot)?);
    Ok(())
}

#[derive(serde::Serialize)]
struct RepoTreeOutput {
    root: String,
    tree: String,
    file_count: usize,
    total_chars: usize,
    total_est_tokens: usize,
    skipped_count: usize,
}

impl crate::output::CommandOutput for RepoTreeOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "{}", self.tree.trim_end())?;
        writeln!(w)?;
        writeln!(
            w,
            "files={} chars={} est_tokens={} skipped={}",
            self.file_count, self.total_chars, self.total_est_tokens, self.skipped_count
        )?;
        Ok(())
    }
}

fn cmd_repo_tree(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let opts = parse_repo_args(args)?;
    let snapshot = build_repo_snapshot(&opts)?;
    let output = RepoTreeOutput {
        root: snapshot.display_root.clone(),
        tree: snapshot.tree.clone(),
        file_count: snapshot.files.len(),
        total_chars: snapshot.total_chars,
        total_est_tokens: snapshot.total_est_tokens,
        skipped_count: snapshot.skipped.len(),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

#[cfg(test)]
mod tests;
