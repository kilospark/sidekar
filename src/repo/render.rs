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

    if !snapshot.skipped.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "## Skipped Files");
        for skipped in &snapshot.skipped {
            let _ = writeln!(out, "- `{}` - {}", skipped.path, skipped.reason);
        }
    }

    out
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

