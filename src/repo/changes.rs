use super::*;

#[derive(Debug)]
pub(super) struct RepoChangesArgs {
    pub(super) target: Option<String>,
    pub(super) since_ref: Option<String>,
    pub(super) max_files: usize,
    pub(super) max_symbols: usize,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub(super) struct RepoSymbol {
    pub(super) name: String,
    pub(super) kind: String,
    pub(super) line: usize,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub(super) struct RepoChangedFile {
    pub(super) path: String,
    pub(super) status: String,
    pub(super) symbols: Vec<RepoSymbol>,
}

#[derive(Debug, Serialize)]
pub(super) struct RepoChangesSummary {
    pub(super) root: PathBuf,
    pub(super) scope: String,
    pub(super) since_ref: Option<String>,
    pub(super) modified_files: usize,
    pub(super) added_files: usize,
    pub(super) deleted_files: usize,
    pub(super) renamed_files: usize,
    pub(super) untracked_files: usize,
    pub(super) reported_files: usize,
    pub(super) remaining_files: usize,
    pub(super) files: Vec<RepoChangedFile>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChangeStatus {
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
pub(super) struct ChangeEntry {
    pub(super) path: String,
    pub(super) status: ChangeStatus,
}

pub(super) fn parse_repo_changes_args(args: &[String]) -> Result<RepoChangesArgs> {
    let mut target = None;
    let mut since_ref = None;
    let mut max_files = DEFAULT_CHANGES_MAX_FILES;
    let mut max_symbols = DEFAULT_CHANGES_MAX_SYMBOLS;

    for arg in args {
        if let Some(value) = arg.strip_prefix("--since=") {
            since_ref = Some(value.to_string());
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
            bail!(
                "Usage: sidekar repo changes [path] [--since=<ref>] [--max-files=N] [--max-symbols=N]"
            );
        }
    }

    Ok(RepoChangesArgs {
        target,
        since_ref,
        max_files,
        max_symbols,
        })
}

pub(super) fn build_repo_changes_summary(args: &RepoChangesArgs) -> Result<RepoChangesSummary> {
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
            &[
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--",
                &scope_spec,
            ],
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
            symbols: summarize_changed_file_symbols(
                git_root,
                &entry.path,
                entry.status,
                args.max_symbols,
            ),
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

pub(super) fn run_git_diff(git_root: &Path, scope_root: &Path) -> Result<String> {
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

pub(super) fn run_git_log(git_root: &Path, scope_root: &Path, limit: usize) -> Result<String> {
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

fn empty_fallback<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

pub(super) fn parse_porcelain_status_output(output: &str) -> Vec<ChangeEntry> {
    let mut entries = Vec::new();
    for line in output
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
    {
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

pub(super) fn parse_name_status_output(output: &str) -> Vec<ChangeEntry> {
    let mut entries = Vec::new();
    for line in output
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
    {
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

pub(super) fn extract_symbol_summaries(
    path: &str,
    content: &str,
    max_symbols: usize,
) -> Vec<RepoSymbol> {
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
            if let Some(caps) = regex.captures(line)
                && let Some(name) = caps.get(1)
            {
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

impl crate::output::CommandOutput for RepoChangesSummary {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "Repo Changes: {}", self.root.display())?;
        writeln!(w, "Scope: {}", self.scope)?;
        writeln!(
            w,
            "Base: {}",
            self.since_ref
                .as_deref()
                .map(|value| format!("diff since {value}"))
                .unwrap_or_else(|| "current worktree".to_string())
        )?;
        writeln!(
            w,
            "modified={} added={} deleted={} renamed={} untracked={} reported={} remaining={}",
            self.modified_files,
            self.added_files,
            self.deleted_files,
            self.renamed_files,
            self.untracked_files,
            self.reported_files,
            self.remaining_files
        )?;
        if self.files.is_empty() {
            writeln!(w)?;
            writeln!(w, "No changes found.")?;
            return Ok(());
        }
        for file in &self.files {
            writeln!(w)?;
            writeln!(w, "- {} {}", file.status, file.path)?;
            if file.symbols.is_empty() {
                writeln!(w, "  symbols: -")?;
            } else {
                writeln!(w, "  symbols:")?;
                for symbol in &file.symbols {
                    writeln!(w, "    - {} {} @{}", symbol.kind, symbol.name, symbol.line)?;
                }
            }
        }
        Ok(())
    }
}
