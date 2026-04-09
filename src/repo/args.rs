use super::*;

pub(super) fn parse_repo_pack_args(args: &[String]) -> Result<RepoPackArgs> {
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

pub(super) fn parse_repo_args(args: &[String]) -> Result<RepoArgs> {
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

pub(super) fn resolve_cli_path(cwd: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

pub(super) fn resolve_project_root(cwd: &Path, target: Option<&str>) -> Result<PathBuf> {
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

pub(super) fn resolve_repo_scope(cwd: &Path, target: Option<&str>) -> Result<RepoScope> {
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

pub(super) fn path_for_git_scope(git_root: &Path, scope_root: &Path) -> String {
    if git_root == scope_root {
        ".".to_string()
    } else {
        normalize_relative_path(scope_root.strip_prefix(git_root).unwrap_or(scope_root))
    }
}

pub(super) fn run_git_capture(git_root: &Path, args: &[&str]) -> Result<String> {
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

pub(super) fn normalize_scope_display(scope_path: &Path, git_root: &Path) -> String {
    if scope_path == git_root {
        ".".to_string()
    } else {
        normalize_relative_path(scope_path.strip_prefix(git_root).unwrap_or(scope_path))
    }
}
