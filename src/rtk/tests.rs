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

#[test]
fn preserves_pytest_stdout_when_capture_is_disabled() {
    let raw = "\
============================= test session starts ==============================
collected 1 item

tests/test_demo.py debug: alpha
tests/test_demo.py debug: beta
.                                                                        [100%]

============================== 1 passed in 0.12s ===============================
";
    let compacted = compact_output("pytest -s", raw);
    assert!(compacted.contains("debug: alpha"));
    assert!(compacted.contains("debug: beta"));
    assert!(compacted.contains("1 passed in 0.12s"));
}

#[test]
fn preserves_runner_prefixed_pytest_stdout_when_capture_is_disabled() {
    let raw = "\
tests/test_demo.py debug: alpha
tests/test_demo.py debug: beta
============================== 1 passed in 0.12s ===============================
";
    let compacted = compact_output("uv run pytest --capture=no", raw);
    assert!(compacted.contains("debug: alpha"));
    assert!(compacted.contains("debug: beta"));
}

#[test]
fn compacts_pytest_output_when_capture_is_enabled() {
    let raw = "\
============================= test session starts ==============================
collected 1 item

tests/test_demo.py .                                                      [100%]

============================== 1 passed in 0.12s ===============================
";
    let compacted = compact_output("pytest", raw);
    assert_eq!(
        compacted,
        "============================== 1 passed in 0.12s ==============================="
    );
}
