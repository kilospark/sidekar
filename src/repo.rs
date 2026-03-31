use crate::*;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

const DEFAULT_MAX_FILE_BYTES: usize = 1_000_000;
const DEFAULT_LOG_COUNT: usize = 10;
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
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn empty_fallback<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
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
}
