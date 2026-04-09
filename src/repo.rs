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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RepoStyle {
    Markdown,
    Json,
    Plain,
}

impl RepoStyle {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "markdown" | "md" => Ok(Self::Markdown),
            "json" => Ok(Self::Json),
            "plain" | "text" | "txt" => Ok(Self::Plain),
            other => bail!("Unsupported repo style: {other}. Valid: markdown, json, plain"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RepoStructuredStyle {
    Json,
    Plain,
}

impl RepoStructuredStyle {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "json" => Ok(Self::Json),
            "plain" | "text" | "txt" => Ok(Self::Plain),
            other => bail!("Unsupported repo style: {other}. Valid: json, plain"),
        }
    }
}

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
    style: RepoStyle,
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
    match opts.style {
        RepoStyle::Markdown => write_output(ctx, &render_markdown(&snapshot)),
        RepoStyle::Json => write_output(ctx, &render_json(&snapshot)?),
        RepoStyle::Plain => write_output(ctx, &render_plain(&snapshot)),
    }
    Ok(())
}

fn cmd_repo_tree(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let opts = parse_repo_args(args)?;
    let snapshot = build_repo_snapshot(&opts, false, None)?;
    out!(ctx, "{}", snapshot.tree.trim_end());
    out!(
        ctx,
        "\nfiles={} chars={} est_tokens={} skipped={}",
        snapshot.files.len(),
        snapshot.total_chars,
        snapshot.total_est_tokens,
        snapshot.skipped.len()
    );
    Ok(())
}

fn cmd_repo_changes(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let opts = parse_repo_changes_args(args)?;
    let summary = build_repo_changes_summary(&opts)?;
    match opts.style {
        RepoStructuredStyle::Json => write_output(ctx, &render_repo_changes_json(&summary)?),
        RepoStructuredStyle::Plain => write_output(ctx, &render_repo_changes_plain(&summary)),
    }
    Ok(())
}

#[cfg(test)]
mod tests;
