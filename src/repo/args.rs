use super::*;

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
