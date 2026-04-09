use super::*;

pub(super) fn build_repo_snapshot(
    args: &RepoArgs,
    include_diff: bool,
    include_logs: Option<usize>,
) -> Result<RepoSnapshot> {
    let cwd = env::current_dir().context("failed to resolve current directory")?;
    let target_path = args
        .target
        .as_ref()
        .map(|value| resolve_cli_path(&cwd, value));
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
        Some(run_git_log(
            git_root.as_deref().unwrap_or(&root),
            &root,
            limit,
        )?)
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

pub(super) fn resolve_scan_root(
    cwd: &Path,
    target: Option<&Path>,
) -> Result<(PathBuf, Vec<PathBuf>)> {
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
                bail!(
                    "Repo target must be a directory or file: {}",
                    path.display()
                );
            }
            Ok((path.to_path_buf(), Vec::new()))
        }
        None => {
            let root = find_repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
            Ok((root, Vec::new()))
        }
    }
}

pub(super) fn find_repo_root(start: &Path) -> Option<PathBuf> {
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

pub(super) struct MatcherSet {
    include: Option<GlobSet>,
    ignore: GlobSet,
}

impl MatcherSet {
    pub(super) fn new(
        root: &Path,
        include_patterns: &[String],
        ignore_patterns: &[String],
    ) -> Result<Self> {
        let include = if include_patterns.is_empty() {
            None
        } else {
            Some(build_globset(include_patterns)?)
        };

        let mut ignores = DEFAULT_IGNORES
            .iter()
            .map(|item| item.to_string())
            .collect::<Vec<_>>();
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

pub(super) struct RepoCollectResult {
    pub(super) files: Vec<RepoFile>,
    pub(super) skipped: Vec<SkippedFile>,
}

pub(super) fn collect_repo_files(
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

pub(super) fn normalize_relative_path(path: &Path) -> String {
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

pub(super) fn build_tree_string(root: &Path, files: &[RepoFile]) -> String {
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
