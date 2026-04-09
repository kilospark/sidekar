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
mod changes;
mod pack;
mod render;

use actions::*;
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
            bail!(
                "Usage: sidekar repo <pack|tree> [path] [--include=...] [--ignore=...] [--stdin]"
            );
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
                bail!(
                    "Repo target must be a directory or file: {}",
                    path.display()
                );
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
                bail!(
                    "Repo target must be a directory or file: {}",
                    path.display()
                );
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
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

fn normalize_scope_display(scope_path: &Path, git_root: &Path) -> String {
    if scope_path == git_root {
        ".".to_string()
    } else {
        normalize_relative_path(scope_path.strip_prefix(git_root).unwrap_or(scope_path))
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
        fs::write(
            root.join("pyproject.toml"),
            "[tool.pytest.ini_options]\naddopts = \"-q\"\n[tool.ruff]\n",
        )?;
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )?;
        fs::write(
            root.join("Makefile"),
            "test:\n\t@echo ok\nlint:\n\t@echo lint\n",
        )?;

        let actions = discover_project_actions(&root)?;
        let ids = actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>();
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
