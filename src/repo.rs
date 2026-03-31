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

#[derive(Debug)]
struct RepoChangesArgs {
    target: Option<String>,
    since_ref: Option<String>,
    style: RepoStructuredStyle,
    max_files: usize,
    max_symbols: usize,
}

#[derive(Debug)]
struct RepoActionsListArgs {
    target: Option<String>,
    style: RepoStructuredStyle,
}

#[derive(Debug)]
struct RepoActionsRunArgs {
    action_id: String,
    target: Option<String>,
    style: RepoStructuredStyle,
    timeout_secs: u64,
    max_output_chars: usize,
    include_output: bool,
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

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
struct RepoSymbol {
    name: String,
    kind: String,
    line: usize,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
struct RepoChangedFile {
    path: String,
    status: String,
    symbols: Vec<RepoSymbol>,
}

#[derive(Debug, Serialize)]
struct RepoChangesSummary {
    root: PathBuf,
    scope: String,
    since_ref: Option<String>,
    modified_files: usize,
    added_files: usize,
    deleted_files: usize,
    renamed_files: usize,
    untracked_files: usize,
    reported_files: usize,
    remaining_files: usize,
    files: Vec<RepoChangedFile>,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
struct ProjectAction {
    id: String,
    kind: String,
    command: Vec<String>,
    source: String,
    description: String,
}

#[derive(Debug, Serialize)]
struct ProjectActionsSummary {
    root: PathBuf,
    actions: Vec<ProjectAction>,
}

#[derive(Debug, Serialize)]
struct CommandRunSummary {
    action_id: String,
    headline: String,
    exit_code: Option<i32>,
    stdout_lines: usize,
    stderr_lines: usize,
    tail: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ProjectActionRunResult {
    ok: bool,
    root: PathBuf,
    action: ProjectAction,
    exit_code: Option<i32>,
    duration_sec: f64,
    timed_out: bool,
    summary: CommandRunSummary,
    error: Option<String>,
    stdout: Option<String>,
    stderr: Option<String>,
}

#[derive(Debug)]
struct RepoScope {
    root: PathBuf,
    git_root: Option<PathBuf>,
    scope_path: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChangeStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
}

impl ChangeStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Modified => "modified",
            Self::Added => "added",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
            Self::Untracked => "untracked",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChangeEntry {
    path: String,
    status: ChangeStatus,
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

fn cmd_repo_actions(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("run") => cmd_repo_actions_run(ctx, &args[1..]),
        _ => cmd_repo_actions_list(ctx, args),
    }
}

fn cmd_repo_actions_list(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let opts = parse_repo_actions_list_args(args)?;
    let cwd = env::current_dir().context("failed to resolve current directory")?;
    let root = resolve_project_root(&cwd, opts.target.as_deref())?;
    let summary = ProjectActionsSummary {
        root: root.clone(),
        actions: discover_project_actions(&root)?,
    };
    match opts.style {
        RepoStructuredStyle::Json => write_output(ctx, &render_project_actions_json(&summary)?),
        RepoStructuredStyle::Plain => write_output(ctx, &render_project_actions_plain(&summary)),
    }
    Ok(())
}

fn cmd_repo_actions_run(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let opts = parse_repo_actions_run_args(args)?;
    let cwd = env::current_dir().context("failed to resolve current directory")?;
    let root = resolve_project_root(&cwd, opts.target.as_deref())?;
    let result = run_project_action(
        &root,
        &opts.action_id,
        opts.timeout_secs,
        opts.max_output_chars,
        opts.include_output,
    )?;
    match opts.style {
        RepoStructuredStyle::Json => write_output(ctx, &render_project_action_run_json(&result)?),
        RepoStructuredStyle::Plain => write_output(ctx, &render_project_action_run_plain(&result)),
    }
    if !result.ok {
        bail!("project action failed: {}", result.summary.headline);
    }
    Ok(())
}

fn parse_repo_pack_args(args: &[String]) -> Result<RepoPackArgs> {
    let mut style = RepoStyle::Markdown;
    let mut diff = false;
    let mut logs = None;
    let mut repo_args = Vec::new();

    for arg in args {
        if let Some(value) = arg.strip_prefix("--style=") {
            style = RepoStyle::parse(value)?;
        } else if arg == "--diff" {
            diff = true;
        } else if arg == "--logs" {
            logs = Some(DEFAULT_LOG_COUNT);
        } else if let Some(value) = arg.strip_prefix("--logs=") {
            logs = Some(
                value
                    .parse::<usize>()
                    .context("--logs must be a positive integer")?,
            );
        } else {
            repo_args.push(arg.clone());
        }
    }

    Ok(RepoPackArgs {
        common: parse_repo_args(&repo_args)?,
        style,
        diff,
        logs,
    })
}

fn parse_repo_changes_args(args: &[String]) -> Result<RepoChangesArgs> {
    let mut target = None;
    let mut since_ref = None;
    let mut style = RepoStructuredStyle::Plain;
    let mut max_files = DEFAULT_CHANGES_MAX_FILES;
    let mut max_symbols = DEFAULT_CHANGES_MAX_SYMBOLS;

    for arg in args {
        if let Some(value) = arg.strip_prefix("--since=") {
            since_ref = Some(value.to_string());
        } else if let Some(value) = arg.strip_prefix("--style=") {
            style = RepoStructuredStyle::parse(value)?;
        } else if let Some(value) = arg.strip_prefix("--max-files=") {
            max_files = value
                .parse::<usize>()
                .context("--max-files must be a positive integer")?;
        } else if let Some(value) = arg.strip_prefix("--max-symbols=") {
            max_symbols = value
                .parse::<usize>()
                .context("--max-symbols must be a positive integer")?;
        } else if arg.starts_with("--") {
            bail!("Unknown flag: {arg}");
        } else if target.is_none() {
            target = Some(arg.clone());
        } else {
            bail!("Usage: sidekar repo changes [path] [--since=<ref>] [--style=json|plain] [--max-files=N] [--max-symbols=N]");
        }
    }

    Ok(RepoChangesArgs {
        target,
        since_ref,
        style,
        max_files,
        max_symbols,
    })
}

fn parse_repo_actions_list_args(args: &[String]) -> Result<RepoActionsListArgs> {
    let mut target = None;
    let mut style = RepoStructuredStyle::Plain;

    for arg in args {
        if let Some(value) = arg.strip_prefix("--style=") {
            style = RepoStructuredStyle::parse(value)?;
        } else if arg.starts_with("--") {
            bail!("Unknown flag: {arg}");
        } else if target.is_none() {
            target = Some(arg.clone());
        } else {
            bail!("Usage: sidekar repo actions [path] [--style=json|plain]");
        }
    }

    Ok(RepoActionsListArgs { target, style })
}

fn parse_repo_actions_run_args(args: &[String]) -> Result<RepoActionsRunArgs> {
    let mut action_id = None;
    let mut target = None;
    let mut style = RepoStructuredStyle::Plain;
    let mut timeout_secs = DEFAULT_ACTION_TIMEOUT_SECS;
    let mut max_output_chars = DEFAULT_ACTION_MAX_OUTPUT_CHARS;
    let mut include_output = false;

    for arg in args {
        if let Some(value) = arg.strip_prefix("--style=") {
            style = RepoStructuredStyle::parse(value)?;
        } else if let Some(value) = arg.strip_prefix("--timeout=") {
            timeout_secs = value
                .parse::<u64>()
                .context("--timeout must be a positive integer")?;
        } else if let Some(value) = arg.strip_prefix("--max-output-chars=") {
            max_output_chars = value
                .parse::<usize>()
                .context("--max-output-chars must be a positive integer")?;
        } else if arg == "--include-output" {
            include_output = true;
        } else if arg.starts_with("--") {
            bail!("Unknown flag: {arg}");
        } else if action_id.is_none() {
            action_id = Some(arg.clone());
        } else if target.is_none() {
            target = Some(arg.clone());
        } else {
            bail!("Usage: sidekar repo actions run <action-id> [path] [--timeout=N] [--max-output-chars=N] [--include-output] [--style=json|plain]");
        }
    }

    Ok(RepoActionsRunArgs {
        action_id: action_id.context("Usage: sidekar repo actions run <action-id> [path] [--timeout=N] [--max-output-chars=N] [--include-output] [--style=json|plain]")?,
        target,
        style,
        timeout_secs,
        max_output_chars,
        include_output,
    })
}

fn parse_repo_args(args: &[String]) -> Result<RepoArgs> {
    let mut target = None;
    let mut include = Vec::new();
    let mut ignore = Vec::new();
    let mut stdin = false;
    let mut max_file_bytes = DEFAULT_MAX_FILE_BYTES;

    for arg in args {
        if let Some(value) = arg.strip_prefix("--include=") {
            include.extend(split_csv_arg(value));
        } else if let Some(value) = arg.strip_prefix("--ignore=") {
            ignore.extend(split_csv_arg(value));
        } else if let Some(value) = arg.strip_prefix("--max-file-bytes=") {
            max_file_bytes = value
                .parse::<usize>()
                .context("--max-file-bytes must be a positive integer")?;
        } else if arg == "--stdin" {
            stdin = true;
        } else if arg.starts_with("--") {
            bail!("Unknown flag: {arg}");
        } else if target.is_none() {
            target = Some(arg.clone());
        } else {
            bail!("Usage: sidekar repo <pack|tree> [path] [--include=...] [--ignore=...] [--stdin]");
        }
    }

    Ok(RepoArgs {
        target,
        include,
        ignore,
        stdin,
        max_file_bytes,
    })
}

fn split_csv_arg(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn build_repo_changes_summary(args: &RepoChangesArgs) -> Result<RepoChangesSummary> {
    let cwd = env::current_dir().context("failed to resolve current directory")?;
    let scope = resolve_repo_scope(&cwd, args.target.as_deref())?;
    let git_root = scope
        .git_root
        .as_ref()
        .context("repo changes requires a git repository")?;
    let scope_spec = path_for_git_scope(git_root, &scope.scope_path);
    let changes = if let Some(since_ref) = &args.since_ref {
        let output = run_git_capture(
            git_root,
            &[
                "--no-pager",
                "diff",
                "--name-status",
                "--find-renames",
                since_ref,
                "--",
                &scope_spec,
            ],
        )?;
        parse_name_status_output(&output)
    } else {
        let output = run_git_capture(
            git_root,
            &["status", "--porcelain=v1", "--untracked-files=all", "--", &scope_spec],
        )?;
        parse_porcelain_status_output(&output)
    };

    let modified_files = changes
        .iter()
        .filter(|entry| entry.status == ChangeStatus::Modified)
        .count();
    let added_files = changes
        .iter()
        .filter(|entry| entry.status == ChangeStatus::Added)
        .count();
    let deleted_files = changes
        .iter()
        .filter(|entry| entry.status == ChangeStatus::Deleted)
        .count();
    let renamed_files = changes
        .iter()
        .filter(|entry| entry.status == ChangeStatus::Renamed)
        .count();
    let untracked_files = changes
        .iter()
        .filter(|entry| entry.status == ChangeStatus::Untracked)
        .count();

    let files = changes
        .iter()
        .take(args.max_files)
        .map(|entry| RepoChangedFile {
            path: entry.path.clone(),
            status: entry.status.as_str().to_string(),
            symbols: summarize_changed_file_symbols(git_root, &entry.path, entry.status, args.max_symbols),
        })
        .collect::<Vec<_>>();

    Ok(RepoChangesSummary {
        root: scope.root,
        scope: normalize_scope_display(&scope.scope_path, git_root),
        since_ref: args.since_ref.clone(),
        modified_files,
        added_files,
        deleted_files,
        renamed_files,
        untracked_files,
        reported_files: files.len(),
        remaining_files: changes.len().saturating_sub(files.len()),
        files,
    })
}

fn build_repo_snapshot(args: &RepoArgs, include_diff: bool, include_logs: Option<usize>) -> Result<RepoSnapshot> {
    let cwd = env::current_dir().context("failed to resolve current directory")?;
    let target_path = args.target.as_ref().map(|value| resolve_cli_path(&cwd, value));
    let (root, explicit_from_target) = resolve_scan_root(&cwd, target_path.as_deref())?;
    let explicit_from_stdin = if args.stdin {
        read_explicit_paths_from_stdin(&cwd, &root)?
    } else {
        Vec::new()
    };

    let matcher = MatcherSet::new(&root, &args.include, &args.ignore)?;
    let explicit_files = explicit_from_target
        .into_iter()
        .chain(explicit_from_stdin)
        .collect::<Vec<_>>();
    let repo_files = collect_repo_files(&root, &matcher, &explicit_files, args.max_file_bytes)?;

    let total_chars = repo_files
        .files
        .iter()
        .map(|file| file.chars)
        .sum::<usize>();
    let total_est_tokens = repo_files
        .files
        .iter()
        .map(|file| file.est_tokens)
        .sum::<usize>();
    let tree = build_tree_string(&root, &repo_files.files);

    let git_root = find_repo_root(&root);
    let git_diff = if include_diff {
        Some(run_git_diff(git_root.as_deref().unwrap_or(&root), &root)?)
    } else {
        None
    };
    let git_log = if let Some(limit) = include_logs {
        Some(run_git_log(git_root.as_deref().unwrap_or(&root), &root, limit)?)
    } else {
        None
    };

    Ok(RepoSnapshot {
        display_root: root
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| root.to_string_lossy().into_owned()),
        root,
        total_chars,
        total_est_tokens,
        files: repo_files.files,
        skipped: repo_files.skipped,
        tree,
        git_diff,
        git_log,
    })
}

fn resolve_cli_path(cwd: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn resolve_project_root(cwd: &Path, target: Option<&str>) -> Result<PathBuf> {
    match target.map(|value| resolve_cli_path(cwd, value)) {
        Some(path) if path.is_file() => {
            let parent = path
                .parent()
                .context("file target must have a parent directory")?;
            Ok(find_repo_root(parent).unwrap_or_else(|| parent.to_path_buf()))
        }
        Some(path) => {
            if !path.exists() {
                bail!("Path does not exist: {}", path.display());
            }
            if !path.is_dir() {
                bail!("Repo target must be a directory or file: {}", path.display());
            }
            Ok(path)
        }
        None => Ok(find_repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf())),
    }
}

fn resolve_repo_scope(cwd: &Path, target: Option<&str>) -> Result<RepoScope> {
    let target_path = target.map(|value| resolve_cli_path(cwd, value));
    match target_path {
        Some(path) if path.is_file() => {
            if !path.exists() {
                bail!("Path does not exist: {}", path.display());
            }
            let parent = path
                .parent()
                .context("file target must have a parent directory")?;
            let git_root = find_repo_root(parent);
            let root = git_root.clone().unwrap_or_else(|| parent.to_path_buf());
            Ok(RepoScope {
                root,
                git_root,
                scope_path: path,
            })
        }
        Some(path) => {
            if !path.exists() {
                bail!("Path does not exist: {}", path.display());
            }
            if !path.is_dir() {
                bail!("Repo target must be a directory or file: {}", path.display());
            }
            let git_root = find_repo_root(&path);
            Ok(RepoScope {
                root: path.clone(),
                git_root,
                scope_path: path,
            })
        }
        None => {
            let root = find_repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
            let git_root = find_repo_root(&root);
            Ok(RepoScope {
                root: root.clone(),
                git_root,
                scope_path: root,
            })
        }
    }
}

fn resolve_scan_root(cwd: &Path, target: Option<&Path>) -> Result<(PathBuf, Vec<PathBuf>)> {
    match target {
        Some(path) if path.is_file() => {
            let parent = path
                .parent()
                .context("file target must have a parent directory")?;
            let root = find_repo_root(parent).unwrap_or_else(|| parent.to_path_buf());
            Ok((root, vec![path.to_path_buf()]))
        }
        Some(path) => {
            if !path.exists() {
                bail!("Path does not exist: {}", path.display());
            }
            if !path.is_dir() {
                bail!("Repo target must be a directory or file: {}", path.display());
            }
            Ok((path.to_path_buf(), Vec::new()))
        }
        None => {
            let root = find_repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
            Ok((root, Vec::new()))
        }
    }
}

fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        let marker = dir.join(".git");
        if marker.is_dir() || marker.is_file() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

fn read_explicit_paths_from_stdin(cwd: &Path, root: &Path) -> Result<Vec<PathBuf>> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("failed to read repo file list from stdin")?;
    Ok(input
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let path = PathBuf::from(line);
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        })
        .filter(|path| path.starts_with(root))
        .collect())
}

struct MatcherSet {
    include: Option<GlobSet>,
    ignore: GlobSet,
}

impl MatcherSet {
    fn new(root: &Path, include_patterns: &[String], ignore_patterns: &[String]) -> Result<Self> {
        let include = if include_patterns.is_empty() {
            None
        } else {
            Some(build_globset(include_patterns)?)
        };

        let mut ignores = DEFAULT_IGNORES.iter().map(|item| item.to_string()).collect::<Vec<_>>();
        ignores.extend(read_ignore_file(&root.join(".sidekarignore"))?);
        ignores.extend(ignore_patterns.iter().cloned());

        Ok(Self {
            include,
            ignore: build_globset(&ignores)?,
        })
    }

    fn matches(&self, relative_path: &str) -> bool {
        if self.ignore.is_match(relative_path) {
            return false;
        }
        match &self.include {
            Some(include) => include.is_match(relative_path),
            None => true,
        }
    }
}

fn build_globset(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let normalized = normalize_glob_pattern(pattern);
        builder.add(
            Glob::new(&normalized).with_context(|| format!("invalid glob pattern: {pattern}"))?,
        );
    }
    builder.build().context("failed to compile globset")
}

fn normalize_glob_pattern(pattern: &str) -> String {
    let trimmed = pattern.trim().trim_start_matches("./");
    if trimmed.ends_with('/') {
        let no_slash = trimmed.trim_end_matches('/');
        format!("{no_slash}/**")
    } else {
        trimmed.to_string()
    }
}

fn read_ignore_file(path: &Path) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read ignore file {}", path.display()))?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect())
}

struct RepoCollectResult {
    files: Vec<RepoFile>,
    skipped: Vec<SkippedFile>,
}

fn collect_repo_files(
    root: &Path,
    matcher: &MatcherSet,
    explicit_files: &[PathBuf],
    max_file_bytes: usize,
) -> Result<RepoCollectResult> {
    let mut files = Vec::new();
    let mut skipped = Vec::new();

    let selected_paths = if explicit_files.is_empty() {
        walk_repo_files(root)?
    } else {
        dedupe_explicit_files(root, explicit_files)?
    };

    for abs_path in selected_paths {
        let relative = match abs_path.strip_prefix(root) {
            Ok(path) => normalize_relative_path(path),
            Err(_) => continue,
        };
        if relative.is_empty() || !matcher.matches(&relative) {
            continue;
        }

        let bytes = match fs::read(&abs_path) {
            Ok(bytes) => bytes,
            Err(err) => {
                skipped.push(SkippedFile {
                    path: relative.clone(),
                    reason: format!("read error: {err}"),
                });
                continue;
            }
        };

        if bytes.len() > max_file_bytes {
            skipped.push(SkippedFile {
                path: relative.clone(),
                reason: format!("skipped large file ({} bytes)", bytes.len()),
            });
            continue;
        }
        if bytes.contains(&0) {
            skipped.push(SkippedFile {
                path: relative.clone(),
                reason: "skipped binary file".to_string(),
            });
            continue;
        }

        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(_) => {
                skipped.push(SkippedFile {
                    path: relative.clone(),
                    reason: "skipped non-UTF-8 file".to_string(),
                });
                continue;
            }
        };
        let chars = content.chars().count();
        files.push(RepoFile {
            path: relative,
            chars,
            est_tokens: estimate_tokens(&content),
            language: language_hint(&abs_path),
            content,
        });
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    skipped.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(RepoCollectResult { files, skipped })
}

fn walk_repo_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut builder = WalkBuilder::new(root);
    builder.hidden(false);
    builder.git_ignore(true);
    builder.git_global(true);
    builder.git_exclude(true);
    builder.ignore(true);
    builder.parents(true);
    builder.follow_links(false);
    builder.require_git(false);

    let mut paths = Vec::new();
    for entry in builder.build() {
        let entry = entry.with_context(|| format!("failed to walk {}", root.display()))?;
        if entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false)
        {
            paths.push(entry.into_path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn dedupe_explicit_files(root: &Path, explicit_files: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut paths = BTreeSet::new();
    for path in explicit_files {
        let canonical = if path.is_absolute() {
            path.clone()
        } else {
            root.join(path)
        };
        if canonical.is_file() && canonical.starts_with(root) {
            paths.insert(canonical);
        }
    }
    Ok(paths.into_iter().collect())
}

fn normalize_relative_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => part.to_str().map(str::to_string),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn estimate_tokens(content: &str) -> usize {
    let chars = content.chars().count();
    (chars + 3) / 4
}

fn language_hint(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("rs") => Some("rust"),
        Some("ts") => Some("ts"),
        Some("tsx") => Some("tsx"),
        Some("js") => Some("js"),
        Some("jsx") => Some("jsx"),
        Some("py") => Some("python"),
        Some("go") => Some("go"),
        Some("java") => Some("java"),
        Some("kt") => Some("kotlin"),
        Some("swift") => Some("swift"),
        Some("sh") => Some("bash"),
        Some("json") => Some("json"),
        Some("yaml") | Some("yml") => Some("yaml"),
        Some("toml") => Some("toml"),
        Some("md") => Some("markdown"),
        Some("html") => Some("html"),
        Some("css") => Some("css"),
        Some("sql") => Some("sql"),
        _ => None,
    }
}

fn build_tree_string(root: &Path, files: &[RepoFile]) -> String {
    let mut tree = TreeNode::default();
    for file in files {
        insert_tree_path(&mut tree, &file.path, file.est_tokens);
    }

    let mut output = String::new();
    let label = root
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| root.to_string_lossy().into_owned());
    let _ = writeln!(
        output,
        "{label}/ (~{} tok, {} files)",
        tree.est_tokens, tree.file_count
    );
    render_tree_children(&tree, "", &mut output);
    output
}

fn insert_tree_path(tree: &mut TreeNode, relative: &str, est_tokens: usize) {
    tree.file_count += 1;
    tree.est_tokens += est_tokens;

    let mut parts = relative.split('/').peekable();
    let mut node = tree;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            node.files.insert(part.to_string(), est_tokens);
        } else {
            node = node.dirs.entry(part.to_string()).or_default();
            node.file_count += 1;
            node.est_tokens += est_tokens;
        }
    }
}

fn render_tree_children(node: &TreeNode, prefix: &str, out: &mut String) {
    let mut entries = Vec::new();
    for (name, child) in &node.dirs {
        entries.push(TreeEntry::Dir(name, child));
    }
    for (name, tokens) in &node.files {
        entries.push(TreeEntry::File(name, *tokens));
    }

    for (idx, entry) in entries.iter().enumerate() {
        let is_last = idx + 1 == entries.len();
        let branch = if is_last { "└── " } else { "├── " };
        match entry {
            TreeEntry::Dir(name, child) => {
                let _ = writeln!(
                    out,
                    "{prefix}{branch}{name}/ (~{} tok, {} files)",
                    child.est_tokens, child.file_count
                );
                let next_prefix = if is_last {
                    format!("{prefix}    ")
                } else {
                    format!("{prefix}│   ")
                };
                render_tree_children(child, &next_prefix, out);
            }
            TreeEntry::File(name, tokens) => {
                let _ = writeln!(out, "{prefix}{branch}{name} (~{tokens} tok)");
            }
        }
    }
}

enum TreeEntry<'a> {
    Dir(&'a String, &'a TreeNode),
    File(&'a String, usize),
}

fn run_git_diff(git_root: &Path, scope_root: &Path) -> Result<String> {
    let relative_scope = path_for_git_scope(git_root, scope_root);
    let worktree = run_git_capture(
        git_root,
        &["--no-pager", "diff", "--no-ext-diff", "--", &relative_scope],
    )?;
    let staged = run_git_capture(
        git_root,
        &[
            "--no-pager",
            "diff",
            "--cached",
            "--no-ext-diff",
            "--",
            &relative_scope,
        ],
    )?;

    Ok(format!(
        "## Worktree Diff\n{}\n\n## Staged Diff\n{}",
        empty_fallback(&worktree, "No unstaged changes."),
        empty_fallback(&staged, "No staged changes.")
    ))
}

fn run_git_log(git_root: &Path, scope_root: &Path, limit: usize) -> Result<String> {
    let relative_scope = path_for_git_scope(git_root, scope_root);
    let limit_str = limit.to_string();
    let output = run_git_capture(
        git_root,
        &[
            "log",
            "--date=short",
            "--pretty=format:%h %ad %s",
            "--name-only",
            "-n",
            &limit_str,
            "--",
            &relative_scope,
        ],
    )?;
    Ok(empty_fallback(&output, "No recent commits in scope.").to_string())
}

fn path_for_git_scope(git_root: &Path, scope_root: &Path) -> String {
    if git_root == scope_root {
        ".".to_string()
    } else {
        normalize_relative_path(scope_root.strip_prefix(git_root).unwrap_or(scope_root))
    }
}

fn run_git_capture(git_root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(git_root)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git in {}", git_root.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git command failed: {}", stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim_end().to_string())
}

fn empty_fallback<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

fn normalize_scope_display(scope_path: &Path, git_root: &Path) -> String {
    if scope_path == git_root {
        ".".to_string()
    } else {
        normalize_relative_path(scope_path.strip_prefix(git_root).unwrap_or(scope_path))
    }
}

fn parse_porcelain_status_output(output: &str) -> Vec<ChangeEntry> {
    let mut entries = Vec::new();
    for line in output.lines().map(str::trim_end).filter(|line| !line.is_empty()) {
        if line.len() < 3 {
            continue;
        }
        let status = &line[..2];
        let rest = line[3..].trim();
        if rest.is_empty() {
            continue;
        }
        let path = rest
            .split(" -> ")
            .last()
            .map(str::trim)
            .unwrap_or(rest)
            .to_string();
        let kind = classify_porcelain_status(status);
        entries.push(ChangeEntry { path, status: kind });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}

fn classify_porcelain_status(status: &str) -> ChangeStatus {
    let mut chars = status.chars();
    let first = chars.next().unwrap_or(' ');
    let second = chars.next().unwrap_or(' ');
    if first == '?' || second == '?' {
        ChangeStatus::Untracked
    } else if first == 'R' || second == 'R' {
        ChangeStatus::Renamed
    } else if first == 'D' || second == 'D' {
        ChangeStatus::Deleted
    } else if first == 'A' || second == 'A' {
        ChangeStatus::Added
    } else {
        ChangeStatus::Modified
    }
}

fn parse_name_status_output(output: &str) -> Vec<ChangeEntry> {
    let mut entries = Vec::new();
    for line in output.lines().map(str::trim_end).filter(|line| !line.is_empty()) {
        let mut parts = line.split('\t');
        let status_part = match parts.next() {
            Some(value) => value,
            None => continue,
        };
        let status_char = status_part.chars().next().unwrap_or('M');
        let status = match status_char {
            'A' => ChangeStatus::Added,
            'D' => ChangeStatus::Deleted,
            'R' => ChangeStatus::Renamed,
            _ => ChangeStatus::Modified,
        };
        let path = if status == ChangeStatus::Renamed {
            parts.nth(1).or_else(|| parts.next())
        } else {
            parts.next()
        };
        let Some(path) = path else { continue };
        entries.push(ChangeEntry {
            path: path.trim().to_string(),
            status,
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}

fn summarize_changed_file_symbols(
    git_root: &Path,
    relative_path: &str,
    status: ChangeStatus,
    max_symbols: usize,
) -> Vec<RepoSymbol> {
    if status == ChangeStatus::Deleted || max_symbols == 0 {
        return Vec::new();
    }
    let absolute = git_root.join(relative_path);
    let Ok(bytes) = fs::read(&absolute) else {
        return Vec::new();
    };
    if bytes.contains(&0) {
        return Vec::new();
    }
    let Ok(content) = String::from_utf8(bytes) else {
        return Vec::new();
    };
    extract_symbol_summaries(relative_path, &content, max_symbols)
}

fn extract_symbol_summaries(path: &str, content: &str, max_symbols: usize) -> Vec<RepoSymbol> {
    match Path::new(path).extension().and_then(|ext| ext.to_str()) {
        Some("rs") => extract_regex_symbols(
            content,
            &[
                ("function", Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)").expect("valid rust fn regex")),
                ("struct", Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)").expect("valid rust struct regex")),
                ("enum", Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)").expect("valid rust enum regex")),
                ("trait", Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?trait\s+([A-Za-z_][A-Za-z0-9_]*)").expect("valid rust trait regex")),
            ],
            max_symbols,
        ),
        Some("py") => extract_regex_symbols(
            content,
            &[
                ("function", Regex::new(r"^\s*(?:async\s+)?def\s+([A-Za-z_][A-Za-z0-9_]*)").expect("valid python def regex")),
                ("class", Regex::new(r"^\s*class\s+([A-Za-z_][A-Za-z0-9_]*)").expect("valid python class regex")),
            ],
            max_symbols,
        ),
        Some("ts") | Some("tsx") | Some("js") | Some("jsx") => extract_regex_symbols(
            content,
            &[
                ("function", Regex::new(r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s+([A-Za-z_][A-Za-z0-9_]*)").expect("valid js function regex")),
                ("variable", Regex::new(r"^\s*(?:export\s+)?(?:const|let|var)\s+([A-Za-z_][A-Za-z0-9_]*)\s*=").expect("valid js variable regex")),
                ("class", Regex::new(r"^\s*(?:export\s+)?class\s+([A-Za-z_][A-Za-z0-9_]*)").expect("valid js class regex")),
                ("interface", Regex::new(r"^\s*(?:export\s+)?interface\s+([A-Za-z_][A-Za-z0-9_]*)").expect("valid ts interface regex")),
                ("type", Regex::new(r"^\s*(?:export\s+)?type\s+([A-Za-z_][A-Za-z0-9_]*)").expect("valid ts type regex")),
            ],
            max_symbols,
        ),
        Some("go") => extract_regex_symbols(
            content,
            &[
                ("function", Regex::new(r"^\s*func\s+(?:\([^)]*\)\s*)?([A-Za-z_][A-Za-z0-9_]*)").expect("valid go func regex")),
                ("type", Regex::new(r"^\s*type\s+([A-Za-z_][A-Za-z0-9_]*)\s+(?:struct|interface)").expect("valid go type regex")),
            ],
            max_symbols,
        ),
        Some("md") | Some("txt") | Some("rst") => extract_heading_symbols(content, max_symbols),
        _ => Vec::new(),
    }
}

fn extract_regex_symbols(
    content: &str,
    patterns: &[(&str, Regex)],
    max_symbols: usize,
) -> Vec<RepoSymbol> {
    let mut symbols = Vec::new();
    for (index, line) in content.lines().enumerate() {
        for (kind, regex) in patterns {
            if let Some(caps) = regex.captures(line) {
                if let Some(name) = caps.get(1) {
                    symbols.push(RepoSymbol {
                        name: name.as_str().to_string(),
                        kind: (*kind).to_string(),
                        line: index + 1,
                    });
                    if symbols.len() >= max_symbols {
                        return symbols;
                    }
                    break;
                }
            }
        }
    }
    symbols
}

fn extract_heading_symbols(content: &str, max_symbols: usize) -> Vec<RepoSymbol> {
    let mut symbols = Vec::new();
    for (index, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if let Some(title) = trimmed.strip_prefix('#') {
            let heading = title.trim_start_matches('#').trim();
            if !heading.is_empty() {
                symbols.push(RepoSymbol {
                    name: heading.to_string(),
                    kind: "section".to_string(),
                    line: index + 1,
                });
                if symbols.len() >= max_symbols {
                    break;
                }
            }
        }
    }
    symbols
}

fn discover_project_actions(root: &Path) -> Result<Vec<ProjectAction>> {
    let mut actions = Vec::new();
    let mut seen = BTreeSet::new();

    let mut add_action = |id: &str, kind: &str, command: Vec<String>, source: &str, description: &str| {
        if seen.insert(id.to_string()) {
            actions.push(ProjectAction {
                id: id.to_string(),
                kind: kind.to_string(),
                command,
                source: source.to_string(),
                description: description.to_string(),
            });
        }
    };

    let package_json = root.join("package.json");
    if package_json.exists() {
        if let Ok(raw) = fs::read_to_string(&package_json) {
            if let Ok(value) = serde_json::from_str::<Value>(&raw) {
                if let Some(scripts) = value.get("scripts").and_then(Value::as_object) {
                    for script_name in scripts.keys().collect::<Vec<_>>() {
                        let kind = match script_name.as_str() {
                            "test" => "test",
                            "lint" => "lint",
                            "build" => "build",
                            "start" | "dev" => "run",
                            "typecheck" | "check" => "check",
                            _ => "custom",
                        };
                        add_action(
                            &format!("npm:{script_name}"),
                            kind,
                            vec!["npm".to_string(), "run".to_string(), script_name.to_string()],
                            "package.json",
                            &format!("Run npm script '{script_name}'."),
                        );
                    }
                }
            }
        }
    }

    let pyproject = root.join("pyproject.toml");
    if pyproject.exists() {
        let raw = fs::read_to_string(&pyproject).unwrap_or_default();
        let has_tests_dir = root.join("tests").is_dir();
        if raw.contains("[tool.pytest") || has_tests_dir {
            let mut command = vec!["pytest".to_string()];
            if has_tests_dir {
                command.push("tests/".to_string());
                command.push("-v".to_string());
            }
            add_action(
                "python:test",
                "test",
                command,
                "pyproject.toml",
                "Run the Python test suite with pytest.",
            );
        }
        if raw.contains("[tool.ruff") {
            let mut command = vec!["ruff".to_string(), "check".to_string()];
            if root.join("src").exists() {
                command.push("src/".to_string());
            }
            if root.join("tests").exists() {
                command.push("tests/".to_string());
            }
            if command.len() == 2 {
                command.push(".".to_string());
            }
            add_action(
                "python:lint",
                "lint",
                command,
                "pyproject.toml",
                "Run Ruff checks for the Python project.",
            );
        }
    }

    if root.join("Cargo.toml").exists() {
        add_action(
            "cargo:test",
            "test",
            vec!["cargo".to_string(), "test".to_string()],
            "Cargo.toml",
            "Run the Rust test suite.",
        );
        add_action(
            "cargo:check",
            "check",
            vec!["cargo".to_string(), "check".to_string()],
            "Cargo.toml",
            "Run cargo check.",
        );
        add_action(
            "cargo:build",
            "build",
            vec!["cargo".to_string(), "build".to_string()],
            "Cargo.toml",
            "Build the Rust project.",
        );
    }

    if root.join("go.mod").exists() {
        add_action(
            "go:test",
            "test",
            vec!["go".to_string(), "test".to_string(), "./...".to_string()],
            "go.mod",
            "Run Go tests.",
        );
        add_action(
            "go:build",
            "build",
            vec!["go".to_string(), "build".to_string(), "./...".to_string()],
            "go.mod",
            "Build Go packages.",
        );
    }

    for makefile_name in ["Makefile", "makefile", "GNUmakefile"] {
        let path = root.join(makefile_name);
        if !path.exists() {
            continue;
        }
        let contents = fs::read_to_string(&path).unwrap_or_default();
        let target_regex =
            Regex::new(r"(?m)^([A-Za-z0-9_.-]+)\s*:").expect("valid makefile target regex");
        let targets = target_regex
            .captures_iter(&contents)
            .filter_map(|caps| caps.get(1).map(|m| m.as_str().to_string()))
            .filter(|target| !target.starts_with('.'))
            .collect::<BTreeSet<_>>();
        for (target, kind) in [
            ("test", "test"),
            ("lint", "lint"),
            ("build", "build"),
            ("run", "run"),
        ] {
            if targets.contains(target) {
                add_action(
                    &format!("make:{target}"),
                    kind,
                    vec!["make".to_string(), target.to_string()],
                    makefile_name,
                    &format!("Run make target '{target}'."),
                );
            }
        }
        break;
    }

    actions.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(actions)
}

fn run_project_action(
    root: &Path,
    action_id: &str,
    timeout_secs: u64,
    max_output_chars: usize,
    include_output: bool,
) -> Result<ProjectActionRunResult> {
    let actions = discover_project_actions(root)?;
    let action = actions
        .into_iter()
        .find(|candidate| candidate.id == action_id)
        .with_context(|| format!("unknown action '{action_id}'"))?;
    let start = Instant::now();
    let mut child = Command::new(&action.command[0])
        .args(&action.command[1..])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run action '{}'", action.id))?;
    let timed_out = loop {
        if child.try_wait()?.is_some() {
            break false;
        }
        if start.elapsed().as_secs() >= timeout_secs {
            let _ = child.kill();
            break true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let output = child
        .wait_with_output()
        .with_context(|| format!("failed to collect output for action '{}'", action.id))?;
    let duration_sec = start.elapsed().as_secs_f64();
    let stdout = truncate_output(&String::from_utf8_lossy(&output.stdout), max_output_chars);
    let stderr = truncate_output(&String::from_utf8_lossy(&output.stderr), max_output_chars);
    let summary = summarize_command_output(&action.id, &stdout, &stderr, output.status.code());
    Ok(ProjectActionRunResult {
        ok: output.status.success() && !timed_out,
        root: root.to_path_buf(),
        action,
        exit_code: output.status.code(),
        duration_sec,
        timed_out,
        summary,
        error: timed_out.then(|| format!("Action timed out after {timeout_secs}s.")),
        stdout: include_output.then_some(stdout),
        stderr: include_output.then_some(stderr),
    })
}

fn truncate_output(value: &str, max_output_chars: usize) -> String {
    if value.len() <= max_output_chars {
        return value.to_string();
    }
    let omitted = value.len().saturating_sub(max_output_chars);
    format!("{}\n... [truncated {omitted} chars]", &value[..max_output_chars])
}

fn summarize_command_output(
    action_id: &str,
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
) -> CommandRunSummary {
    let stdout_lines = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let stderr_lines = stderr
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let headline = stdout_lines
        .last()
        .cloned()
        .or_else(|| stderr_lines.last().cloned())
        .unwrap_or_else(|| {
            if exit_code == Some(0) {
                "Command completed successfully.".to_string()
            } else {
                "Command failed.".to_string()
            }
        });
    let tail = stdout_lines
        .iter()
        .chain(stderr_lines.iter())
        .rev()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();

    CommandRunSummary {
        action_id: action_id.to_string(),
        headline,
        exit_code,
        stdout_lines: stdout_lines.len(),
        stderr_lines: stderr_lines.len(),
        tail,
    }
}

fn render_repo_changes_plain(summary: &RepoChangesSummary) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Repo Changes: {}", summary.root.display());
    let _ = writeln!(out, "Scope: {}", summary.scope);
    let _ = writeln!(
        out,
        "Base: {}",
        summary
            .since_ref
            .as_deref()
            .map(|value| format!("diff since {value}"))
            .unwrap_or_else(|| "current worktree".to_string())
    );
    let _ = writeln!(
        out,
        "modified={} added={} deleted={} renamed={} untracked={} reported={} remaining={}",
        summary.modified_files,
        summary.added_files,
        summary.deleted_files,
        summary.renamed_files,
        summary.untracked_files,
        summary.reported_files,
        summary.remaining_files
    );
    if summary.files.is_empty() {
        let _ = writeln!(out, "\nNo changes found.");
        return out;
    }
    for file in &summary.files {
        let _ = writeln!(out, "\n- {} {}", file.status, file.path);
        if file.symbols.is_empty() {
            let _ = writeln!(out, "  symbols: -");
        } else {
            let _ = writeln!(out, "  symbols:");
            for symbol in &file.symbols {
                let _ = writeln!(out, "    - {} {} @{}", symbol.kind, symbol.name, symbol.line);
            }
        }
    }
    out
}

fn render_repo_changes_json(summary: &RepoChangesSummary) -> Result<String> {
    serde_json::to_string_pretty(summary).context("failed to render repo changes JSON")
}

fn render_project_actions_plain(summary: &ProjectActionsSummary) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Project Actions: {}", summary.root.display());
    let _ = writeln!(out, "Actions: {}", summary.actions.len());
    if summary.actions.is_empty() {
        let _ = writeln!(out, "\nNo actions discovered.");
        return out;
    }
    for action in &summary.actions {
        let _ = writeln!(
            out,
            "\n- {} [{}] ({})",
            action.id, action.kind, action.source
        );
        let _ = writeln!(out, "  cmd: {}", action.command.join(" "));
        let _ = writeln!(out, "  {}", action.description);
    }
    out
}

fn render_project_actions_json(summary: &ProjectActionsSummary) -> Result<String> {
    serde_json::to_string_pretty(summary).context("failed to render project actions JSON")
}

fn render_project_action_run_plain(result: &ProjectActionRunResult) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Project Action: {}", result.action.id);
    let _ = writeln!(out, "Root: {}", result.root.display());
    let _ = writeln!(out, "Command: {}", result.action.command.join(" "));
    let _ = writeln!(out, "Exit Code: {:?}", result.exit_code);
    let _ = writeln!(out, "Duration: {:.3}s", result.duration_sec);
    let _ = writeln!(out, "Headline: {}", result.summary.headline);
    let _ = writeln!(
        out,
        "stdout_lines={} stderr_lines={}",
        result.summary.stdout_lines, result.summary.stderr_lines
    );
    if !result.summary.tail.is_empty() {
        let _ = writeln!(out, "Tail:");
        for line in &result.summary.tail {
            let _ = writeln!(out, "- {line}");
        }
    }
    if let Some(stdout) = &result.stdout {
        let _ = writeln!(out, "\nStdout\n{stdout}");
    }
    if let Some(stderr) = &result.stderr {
        let _ = writeln!(out, "\nStderr\n{stderr}");
    }
    out
}

fn render_project_action_run_json(result: &ProjectActionRunResult) -> Result<String> {
    serde_json::to_string_pretty(result).context("failed to render project action result JSON")
}

fn render_markdown(snapshot: &RepoSnapshot) -> String {
    let fence = markdown_fence(snapshot);
    let mut out = String::new();
    let _ = writeln!(out, "# Repo Pack: {}", snapshot.display_root);
    let _ = writeln!(out);
    let _ = writeln!(out, "- Root: `{}`", snapshot.root.display());
    let _ = writeln!(out, "- Files: {}", snapshot.files.len());
    let _ = writeln!(out, "- Characters: {}", snapshot.total_chars);
    let _ = writeln!(out, "- Estimated Tokens: {}", snapshot.total_est_tokens);
    let _ = writeln!(out, "- Skipped: {}", snapshot.skipped.len());
    let _ = writeln!(out);
    let _ = writeln!(out, "## Directory Tree");
    let _ = writeln!(out);
    let _ = writeln!(out, "```text");
    let _ = writeln!(out, "{}", snapshot.tree.trim_end());
    let _ = writeln!(out, "```");

    if !snapshot.files.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "## Files");
        for file in &snapshot.files {
            let _ = writeln!(out);
            let _ = writeln!(
                out,
                "### `{}` (~{} tokens, {} chars)",
                file.path, file.est_tokens, file.chars
            );
            let _ = writeln!(out);
            match file.language {
                Some(language) => {
                    let _ = writeln!(out, "{fence}{language}", fence = fence);
                }
                None => {
                    let _ = writeln!(out, "{fence}", fence = fence);
                }
            }
            let _ = writeln!(out, "{}", file.content);
            let _ = writeln!(out, "{fence}", fence = fence);
        }
    }

    if let Some(diff) = &snapshot.git_diff {
        let _ = writeln!(out);
        let _ = writeln!(out, "## Git Diff");
        let _ = writeln!(out);
        let _ = writeln!(out, "```diff");
        let _ = writeln!(out, "{diff}");
        let _ = writeln!(out, "```");
    }

    if let Some(logs) = &snapshot.git_log {
        let _ = writeln!(out);
        let _ = writeln!(out, "## Git Log");
        let _ = writeln!(out);
        let _ = writeln!(out, "```text");
        let _ = writeln!(out, "{logs}");
        let _ = writeln!(out, "```");
    }

    if !snapshot.skipped.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "## Skipped Files");
        for skipped in &snapshot.skipped {
            let _ = writeln!(out, "- `{}` - {}", skipped.path, skipped.reason);
        }
    }

    out
}

fn render_plain(snapshot: &RepoSnapshot) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Repo Pack: {}", snapshot.display_root);
    let _ = writeln!(out, "Root: {}", snapshot.root.display());
    let _ = writeln!(out, "Files: {}", snapshot.files.len());
    let _ = writeln!(out, "Characters: {}", snapshot.total_chars);
    let _ = writeln!(out, "Estimated Tokens: {}", snapshot.total_est_tokens);
    let _ = writeln!(out, "Skipped: {}", snapshot.skipped.len());
    let _ = writeln!(out);
    let _ = writeln!(out, "Directory Tree");
    let _ = writeln!(out, "{}", snapshot.tree.trim_end());

    if !snapshot.files.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Files");
        for file in &snapshot.files {
            let _ = writeln!(
                out,
                "\n=== {} (~{} tokens, {} chars) ===",
                file.path, file.est_tokens, file.chars
            );
            let _ = writeln!(out, "{}", file.content);
        }
    }

    if let Some(diff) = &snapshot.git_diff {
        let _ = writeln!(out, "\nGit Diff\n{diff}");
    }
    if let Some(logs) = &snapshot.git_log {
        let _ = writeln!(out, "\nGit Log\n{logs}");
    }
    if !snapshot.skipped.is_empty() {
        let _ = writeln!(out, "\nSkipped Files");
        for skipped in &snapshot.skipped {
            let _ = writeln!(out, "- {}: {}", skipped.path, skipped.reason);
        }
    }

    out
}

fn render_json(snapshot: &RepoSnapshot) -> Result<String> {
    let files = snapshot
        .files
        .iter()
        .map(|file| {
            json!({
                "path": file.path,
                "chars": file.chars,
                "est_tokens": file.est_tokens,
                "language": file.language,
                "content": file.content,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string_pretty(&json!({
        "root": snapshot.root,
        "display_root": snapshot.display_root,
        "total_files": snapshot.files.len(),
        "total_chars": snapshot.total_chars,
        "total_est_tokens": snapshot.total_est_tokens,
        "tree": snapshot.tree,
        "files": files,
        "skipped": snapshot.skipped,
        "git_diff": snapshot.git_diff,
        "git_log": snapshot.git_log,
    }))
    .context("failed to render repo JSON")
}

fn markdown_fence(snapshot: &RepoSnapshot) -> String {
    let mut max_ticks = 3usize;
    for file in &snapshot.files {
        for line in file.content.lines() {
            let ticks = line.chars().take_while(|ch| *ch == '`').count();
            if ticks >= max_ticks {
                max_ticks = ticks + 1;
            }
        }
    }
    "`".repeat(max_ticks)
}

fn write_output(ctx: &mut AppContext, content: &str) {
    ctx.output.push_str(content);
    if !content.ends_with('\n') {
        ctx.output.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let mut bytes = [0u8; 8];
        rand::rng().fill_bytes(&mut bytes);
        env::temp_dir().join(format!(
            "sidekar-repo-{name}-{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ))
    }

    #[test]
    fn finds_repo_root_from_subdir() -> Result<()> {
        let root = temp_dir("root");
        fs::create_dir_all(root.join(".git"))?;
        fs::create_dir_all(root.join("nested/deeper"))?;
        let found = find_repo_root(&root.join("nested/deeper")).context("missing root")?;
        assert_eq!(found, root);
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn collects_files_respecting_sidekarignore() -> Result<()> {
        let root = temp_dir("collect");
        fs::create_dir_all(root.join(".git"))?;
        fs::write(root.join(".gitignore"), "ignored.txt\n")?;
        fs::write(root.join(".sidekarignore"), "private/**\n")?;
        fs::write(root.join("keep.rs"), "fn main() {}\n")?;
        fs::write(root.join("ignored.txt"), "nope\n")?;
        fs::create_dir_all(root.join("private"))?;
        fs::write(root.join("private/secret.md"), "secret\n")?;

        let matcher = MatcherSet::new(&root, &[], &[])?;
        let snapshot = collect_repo_files(&root, &matcher, &[], DEFAULT_MAX_FILE_BYTES)?;
        let paths = snapshot
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>();
        assert!(paths.contains(&"keep.rs"));
        assert!(!paths.contains(&"ignored.txt"));
        assert!(!paths.contains(&"private/secret.md"));

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn tree_reports_estimated_tokens() {
        let root = PathBuf::from("/tmp/example");
        let files = vec![
            RepoFile {
                path: "src/main.rs".into(),
                chars: 16,
                est_tokens: 4,
                language: Some("rust"),
                content: "fn main() {}\n".into(),
            },
            RepoFile {
                path: "README.md".into(),
                chars: 20,
                est_tokens: 5,
                language: Some("markdown"),
                content: "# Example\n".into(),
            },
        ];

        let tree = build_tree_string(&root, &files);
        assert!(tree.contains("example/ (~9 tok, 2 files)"));
        assert!(tree.contains("src/ (~4 tok, 1 files)"));
        assert!(tree.contains("main.rs (~4 tok)"));
    }

    #[test]
    fn discovers_project_actions_from_common_files() -> Result<()> {
        let root = temp_dir("actions");
        fs::create_dir_all(root.join("tests"))?;
        fs::write(
            root.join("package.json"),
            r#"{"scripts":{"test":"vitest","lint":"eslint .","build":"next build"}}"#,
        )?;
        fs::write(root.join("pyproject.toml"), "[tool.pytest.ini_options]\naddopts = \"-q\"\n[tool.ruff]\n")?;
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n")?;
        fs::write(root.join("Makefile"), "test:\n\t@echo ok\nlint:\n\t@echo lint\n")?;

        let actions = discover_project_actions(&root)?;
        let ids = actions.iter().map(|action| action.id.as_str()).collect::<Vec<_>>();
        assert!(ids.contains(&"npm:test"));
        assert!(ids.contains(&"npm:lint"));
        assert!(ids.contains(&"npm:build"));
        assert!(ids.contains(&"python:test"));
        assert!(ids.contains(&"python:lint"));
        assert!(ids.contains(&"cargo:test"));
        assert!(ids.contains(&"cargo:check"));
        assert!(ids.contains(&"cargo:build"));
        assert!(ids.contains(&"make:test"));
        assert!(ids.contains(&"make:lint"));

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn parses_git_status_outputs() {
        let porcelain = parse_porcelain_status_output(
            " M src/main.rs\nA  src/new.rs\nD  src/old.rs\nR  src/old_name.rs -> src/new_name.rs\n?? notes.txt\n",
        );
        assert_eq!(
            porcelain,
            vec![
                ChangeEntry {
                    path: "notes.txt".into(),
                    status: ChangeStatus::Untracked,
                },
                ChangeEntry {
                    path: "src/main.rs".into(),
                    status: ChangeStatus::Modified,
                },
                ChangeEntry {
                    path: "src/new.rs".into(),
                    status: ChangeStatus::Added,
                },
                ChangeEntry {
                    path: "src/new_name.rs".into(),
                    status: ChangeStatus::Renamed,
                },
                ChangeEntry {
                    path: "src/old.rs".into(),
                    status: ChangeStatus::Deleted,
                },
            ]
        );

        let diff = parse_name_status_output(
            "M\tsrc/main.rs\nA\tsrc/new.rs\nD\tsrc/old.rs\nR100\tsrc/old_name.rs\tsrc/new_name.rs\n",
        );
        assert_eq!(
            diff,
            vec![
                ChangeEntry {
                    path: "src/main.rs".into(),
                    status: ChangeStatus::Modified,
                },
                ChangeEntry {
                    path: "src/new.rs".into(),
                    status: ChangeStatus::Added,
                },
                ChangeEntry {
                    path: "src/new_name.rs".into(),
                    status: ChangeStatus::Renamed,
                },
                ChangeEntry {
                    path: "src/old.rs".into(),
                    status: ChangeStatus::Deleted,
                },
            ]
        );
    }

    #[test]
    fn extracts_symbol_summaries_from_rust_source() {
        let symbols = extract_symbol_summaries(
            "src/lib.rs",
            "pub struct App {}\nasync fn start() {}\npub trait Runner {}\n",
            10,
        );
        assert_eq!(
            symbols,
            vec![
                RepoSymbol {
                    name: "App".into(),
                    kind: "struct".into(),
                    line: 1,
                },
                RepoSymbol {
                    name: "start".into(),
                    kind: "function".into(),
                    line: 2,
                },
                RepoSymbol {
                    name: "Runner".into(),
                    kind: "trait".into(),
                    line: 3,
                },
            ]
        );
    }
}
