use super::*;

#[test]
fn classifies_git_status() {
    let result = classify_command("git status");
    assert!(matches!(
        result,
        Classification::Supported {
            category: "Git",
            ..
        }
    ));
}

#[test]
fn compacts_git_status_output() {
    let raw = "\
On branch main
Your branch is up to date with 'origin/main'.

Changes not staged for commit:
  (use \"git add <file>...\" to update what will be committed)
  modified:   src/main.rs

Untracked files:
  (use \"git add <file>...\" to include in what will be committed)
  new-file.rs
";
    let compacted = compact_output("git status", raw);
    assert!(compacted.contains("modified:   src/main.rs"));
    assert!(compacted.contains("new-file.rs"));
    assert!(!compacted.contains("On branch"));
}
