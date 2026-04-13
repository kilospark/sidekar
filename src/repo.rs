use crate::*;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::time::Instant;

const DEFAULT_MAX_FILE_BYTES: usize = 1_000_000;
const DEFAULT_LOG_COUNT: usize = 10;
const DEFAULT_CHANGES_MAX_FILES: usize = 20;
const DEFAULT_CHANGES_MAX_SYMBOLS: usize = 20;
const DEFAULT_ACTION_TIMEOUT_SECS: u64 = 120;
const DEFAULT_ACTION_MAX_OUTPUT_CHARS: usize = 12_000;
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

mod actions;
mod args;
mod changes;
mod pack;
mod render;

use actions::*;
use args::*;
use changes::*;
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

#[derive(Debug)]
struct RepoPackArgs {
    common: RepoArgs,
    diff: bool,
    logs: Option<usize>,
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
    git_diff: Option<String>,
    git_log: Option<String>,
}

impl crate::output::CommandOutput for RepoSnapshot {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        write!(w, "{}", render_markdown(self))
    }
}

#[derive(Debug)]
struct RepoScope {
    root: PathBuf,
    git_root: Option<PathBuf>,
    scope_path: PathBuf,
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
        "changes" => cmd_repo_changes(ctx, &args[1..]),
        "actions" => cmd_repo_actions(ctx, &args[1..]),
        "" => bail!("Usage: sidekar repo <pack|tree|changes|actions> ..."),
        other => bail!("Unknown repo subcommand: {other}"),
    }
}

fn cmd_repo_pack(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let opts = parse_repo_pack_args(args)?;
    let snapshot = build_repo_snapshot(&opts.common, opts.diff, opts.logs)?;
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
    let snapshot = build_repo_snapshot(&opts, false, None)?;
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

fn cmd_repo_changes(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let opts = parse_repo_changes_args(args)?;
    let summary = build_repo_changes_summary(&opts)?;
    out!(ctx, "{}", crate::output::to_string(&summary)?);
    Ok(())
}

#[cfg(test)]
mod tests;
