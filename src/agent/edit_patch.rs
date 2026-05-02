//! Apply a unified-diff (`git diff` style) patch to a single file's contents.
//!
//! Supports one file section (`---` / `+++` pair) with one or more hunks.
//! Intended for agent `Edit` calls when emitting a compact diff beats repeating
//! large verbatim spans.

use anyhow::{Context as _, Result, bail};
use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone)]
enum DiffLine {
    Context(String),
    Remove(String),
    Add(String),
}

#[derive(Debug, Clone)]
struct Hunk {
    /// 1-based line in the original file where this hunk starts (`@@ -n,...`).
    old_start: usize,
    lines: Vec<DiffLine>,
}

static HUNK_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").expect("hunk header regex")
});

fn normalize_line(s: &str) -> String {
    s.trim_end_matches('\r').to_string()
}

/// Strip leading `diff --git`, index lines, etc., until the first `---` line.
fn find_patch_body(patch_text: &str) -> Result<&str> {
    let text = patch_text.trim_start_matches('\u{feff}');
    if text.is_empty() {
        bail!("patch: empty unified diff");
    }
    if text.starts_with("--- ") {
        return Ok(text);
    }
    if let Some(pos) = text.find("\n--- ") {
        return Ok(text[pos + 1..].trim_start());
    }
    bail!("patch: no `--- ` header found (expected unified diff)");
}

fn parse_patch(patch_text: &str) -> Result<Vec<Hunk>> {
    let body = find_patch_body(patch_text)?;
    let mut lines = body.lines().peekable();

    let dash = lines.next().context("patch: truncated after preamble")?;
    if !dash.starts_with("--- ") {
        bail!(
            "patch: expected `--- ` line, got {:?}",
            dash.chars().take(40).collect::<String>()
        );
    }
    if dash.contains("/dev/null") {
        bail!("patch: creating new files is not supported here — use Write");
    }

    let plus = lines.next().context("patch: missing `+++` line after `---`")?;
    if !plus.starts_with("+++ ") {
        bail!(
            "patch: expected `+++ ` line after `---`, got {:?}",
            plus.chars().take(40).collect::<String>()
        );
    }

    let mut hunks = Vec::new();

    loop {
        match lines.peek().copied() {
            None => break,
            Some(l) if l.starts_with("--- ") => {
                bail!(
                    "patch: multiple files in one diff are not supported — split into separate Edit calls"
                );
            }
            Some(l) if l.starts_with("diff ") || l.starts_with("index ") => {
                bail!("patch: unexpected preamble line inside patch body");
            }
            Some(l) if l.starts_with("@@ ") => {
                let hdr_line = lines.next().unwrap();
                let caps = HUNK_HEADER
                    .captures(hdr_line)
                    .with_context(|| format!("patch: bad hunk header: {hdr_line:?}"))?;

                let old_start: usize = caps[1].parse().context("patch: old_start parse")?;

                let mut hunk_lines: Vec<DiffLine> = Vec::new();

                while let Some(ln) = lines.peek().copied() {
                    if ln.starts_with("@@ ") || ln.starts_with("--- ") {
                        break;
                    }
                    let ln = lines.next().unwrap();

                    // `\ No newline at end of file` — skip for MVP (binary-ish edge cases).
                    if ln.starts_with('\\') {
                        continue;
                    }

                    let Some(kind) = ln.chars().next() else {
                        continue;
                    };
                    let rest = normalize_line(ln.get(1..).unwrap_or(""));
                    match kind {
                        ' ' => hunk_lines.push(DiffLine::Context(rest)),
                        '-' => hunk_lines.push(DiffLine::Remove(rest)),
                        '+' => hunk_lines.push(DiffLine::Add(rest)),
                        _ => bail!(
                            "patch: invalid hunk line (must start with ` `, `-`, or `+`): {:?}",
                            ln.chars().take(60).collect::<String>()
                        ),
                    }
                }

                if hunk_lines.is_empty() && old_start > 0 {
                    bail!(
                        "patch: empty hunk at @@ line {:?}",
                        hdr_line.chars().take(80).collect::<String>()
                    );
                }

                hunks.push(Hunk {
                    old_start,
                    lines: hunk_lines,
                });
            }
            Some(other) => {
                bail!(
                    "patch: expected `@@ ` hunk header, got {:?}",
                    other.chars().take(60).collect::<String>()
                );
            }
        }
    }

    if hunks.is_empty() {
        bail!("patch: no hunks found after headers");
    }

    Ok(hunks)
}

fn apply_one_hunk(lines: &mut Vec<String>, hunk: &Hunk) -> Result<()> {
    let mut idx = if hunk.old_start == 0 {
        0usize
    } else {
        hunk.old_start.saturating_sub(1)
    };

    for dl in &hunk.lines {
        match dl {
            DiffLine::Context(expect) => {
                let got = lines
                    .get(idx)
                    .with_context(|| format!("patch: context beyond EOF at line {}", idx + 1))?;
                if got != expect {
                    bail!(
                        "patch: context mismatch at line {}:\nexpected: {:?}\nactual:   {:?}",
                        idx + 1,
                        expect,
                        got
                    );
                }
                idx += 1;
            }
            DiffLine::Remove(expect) => {
                let got = lines
                    .get(idx)
                    .with_context(|| format!("patch: remove beyond EOF at line {}", idx + 1))?;
                if got != expect {
                    bail!(
                        "patch: remove mismatch at line {}:\nexpected: {:?}\nactual:   {:?}",
                        idx + 1,
                        expect,
                        got
                    );
                }
                lines.remove(idx);
            }
            DiffLine::Add(s) => {
                lines.insert(idx, s.clone());
                idx += 1;
            }
        }
    }

    Ok(())
}

/// Apply `patch_text` (unified diff) to full file contents `original`.
pub(crate) fn apply_unified_patch(original: &str, patch_text: &str) -> Result<String> {
    let hunks = parse_patch(patch_text)?;
    let mut lines: Vec<String> = original.lines().map(normalize_line).collect();

    let mut indexed: Vec<(usize, Hunk)> = hunks.into_iter().enumerate().collect();
    indexed.sort_by_key(|(_, h)| std::cmp::Reverse(h.old_start));

    for (_, hunk) in indexed {
        apply_one_hunk(&mut lines, &hunk)?;
    }

    let ends_with_nl = original.ends_with('\n');
    let mut out = lines.join("\n");
    if ends_with_nl && (!out.is_empty() || original.is_empty()) {
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_single_replace() {
        let orig = "fn foo() {\n    1\n}\n";
        let patch = "\
--- a/x.rs
+++ b/x.rs
@@ -1,3 +1,3 @@
 fn foo() {
-    1
+    2
 }
";
        let got = apply_unified_patch(orig, patch).unwrap();
        assert!(got.contains("    2"), "{got:?}");
        assert!(!got.contains("\n    1\n"), "{got:?}");
    }

    #[test]
    fn patch_two_hunks_reverse_order_stable() {
        let orig = "a\nb\nc\nd\ne\n";
        let patch = "\
--- a/t.txt
+++ b/t.txt
@@ -4,2 +4,2 @@
 d
-e
+changed
+z
@@ -1,2 +1,2 @@
-a
+a2
 b
";
        let got = apply_unified_patch(orig, patch).unwrap();
        assert!(got.starts_with("a2\n"), "{got:?}");
        assert!(got.contains("changed\n"), "{got:?}");
        assert!(got.contains("z\n"), "{got:?}");
        assert!(!got.contains("\na\n"), "{got:?}");
    }

    #[test]
    fn patch_with_git_preamble() {
        let orig = "hello\n";
        let patch = "\
diff --git a/foo b/foo
index 111..222 100644
--- a/foo
+++ b/foo
@@ -1,1 +1,1 @@
-hello
+world
";
        let got = apply_unified_patch(orig, patch).unwrap();
        assert_eq!(got, "world\n");
    }

    #[test]
    fn patch_rejects_multi_file() {
        let orig = "x\n";
        let patch = "\
--- a/1.txt
+++ b/1.txt
@@ -1,1 +1,1 @@
-x
+y
--- a/2.txt
+++ b/2.txt
@@ -1,1 +1,1 @@
-a
+b
";
        assert!(apply_unified_patch(orig, patch).is_err());
    }
}
