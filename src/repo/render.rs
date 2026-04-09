use super::*;

pub(super) fn render_markdown(snapshot: &RepoSnapshot) -> String {
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

pub(super) fn render_plain(snapshot: &RepoSnapshot) -> String {
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

pub(super) fn render_json(snapshot: &RepoSnapshot) -> Result<String> {
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

pub(super) fn write_output(ctx: &mut AppContext, content: &str) {
    ctx.output.push_str(content);
    if !content.ends_with('\n') {
        ctx.output.push('\n');
    }
}
