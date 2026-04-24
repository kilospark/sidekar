use super::execute;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

#[tokio::test]
async fn bash_tool_cancels_promptly() {
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_setter = cancel.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel_setter.store(true, Ordering::Relaxed);
    });

    let started = Instant::now();
    let result = execute(
        "bash",
        &json!({
            "command": "sleep 5",
            "timeout": 10
        }),
        Some(&cancel),
    )
    .await;

    assert!(result.is_err());
    assert!(
        result
            .expect_err("cancelled tool")
            .is::<crate::agent::Cancelled>()
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

/// Regression: on cancel, the *entire* subprocess tree must die, not just
/// the bash shell. Before the setpgid + SIGTERM-group fix, `bash -c
/// "sleep 30 &"` would leave the `sleep` reparented to init and still
/// running after the agent turn unwound, making Esc/Ctrl+C feel broken.
///
/// We test this by having bash spawn a child that writes its pid to a
/// temp file, cancelling, and then checking the pid no longer exists.
#[cfg(unix)]
#[tokio::test]
async fn bash_cancel_kills_grandchild_process() {
    let tmpdir = std::env::temp_dir();
    let pidfile = tmpdir.join(format!(
        "sidekar_cancel_test_{}.pid",
        std::process::id()
    ));
    let pidfile_str = pidfile.to_string_lossy().to_string();
    // Ensure stale file doesn't confuse the test.
    let _ = std::fs::remove_file(&pidfile);

    // bash spawns a child `sleep 30` in its own subprocess, writes the
    // grandchild's pid, then waits so bash itself is still alive when we
    // cancel. Use `exec` in a subshell so `$!` points to the real sleep
    // pid rather than an intermediate subshell wrapper.
    let command = format!(
        "( exec sleep 30 ) & echo $! > {pidfile_str}; wait"
    );

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_setter = cancel.clone();
    // Cancel after enough time for bash to have written the pid file.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel_setter.store(true, Ordering::Relaxed);
    });

    let _ = execute(
        "bash",
        &json!({ "command": command, "timeout": 10 }),
        Some(&cancel),
    )
    .await;

    // Give the tree-kill escalation its 500ms + safety margin.
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let pid_str = std::fs::read_to_string(&pidfile)
        .expect("pid file should have been written by bash");
    let pid: i32 = pid_str.trim().parse().expect("pid file contents are a number");
    let _ = std::fs::remove_file(&pidfile);

    // kill(pid, 0) probes existence without sending a signal. Returns
    // 0 if alive, -1 (ESRCH) if the pid no longer exists. On macOS a
    // reaped zombie also returns ESRCH to a non-parent probe, which is
    // fine for our check.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    assert!(
        !alive,
        "grandchild pid {pid} survived cancel — setpgid/SIGTERM-group tree-kill didn't propagate"
    );
}

/// Regression: the Bash tool must actually capture stdout and
/// return it to the caller. The tree-kill refactor switched from
/// `Command::output()` to `Command::spawn()` + `wait_with_output`,
/// which requires explicit `Stdio::piped()` on stdout/stderr —
/// without it, the child inherits the parent's stdio, output
/// flushes to wherever the REPL runs, and `wait_with_output`
/// returns empty Vec<u8>s. Symptom: every bash tool call returned
/// the literal string "(no output)" regardless of what the
/// command printed.
///
/// The fix lives in `run_subprocess_cancellable` (unix arm): set
/// `.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio
/// ::null())` on the Command before spawning.
#[tokio::test]
async fn bash_tool_captures_stdout() {
    let result = execute(
        "bash",
        &json!({ "command": "echo hello-sidekar", "timeout": 5 }),
        None,
    )
    .await
    .expect("bash tool call should succeed");
    assert!(
        result.contains("hello-sidekar"),
        "expected stdout to contain the echoed token, got {result:?}"
    );
    assert!(
        !result.contains("(no output)"),
        "stdio-capture regression — bash returned the empty-output sentinel"
    );
}

/// Regression companion to `bash_tool_captures_stdout`: stderr
/// must be captured too. Same root cause if it regresses.
#[tokio::test]
async fn bash_tool_captures_stderr() {
    let result = execute(
        "bash",
        &json!({
            "command": "echo only-stderr 1>&2; exit 0",
            "timeout": 5
        }),
        None,
    )
    .await
    .expect("bash tool call should succeed");
    assert!(
        result.contains("only-stderr"),
        "expected stderr to be captured, got {result:?}"
    );
}

/// Regression: stdin must be /dev/null (Stdio::null), not
/// inherited. If inherited, commands that read from stdin when it
/// sees a tty — git (pager), cat (no args), less, more — hang
/// waiting for user input that never arrives. The pattern that
/// broke REPL usage was `git log` silently launching `less` and
/// timing out the tool after 120s.
///
/// We test with `cat` (no args) at a tight timeout: with
/// Stdio::null stdin, cat sees immediate EOF and exits 0; with
/// inherited stdin, cat blocks until the test harness's tokio
/// runtime tears down or the tool's own timeout fires.
#[tokio::test]
async fn bash_tool_stdin_is_null_not_inherited() {
    let started = Instant::now();
    let result = execute(
        "bash",
        &json!({ "command": "cat", "timeout": 3 }),
        None,
    )
    .await;
    // Must complete within the test's patience window —
    // comfortably under the 3s tool timeout.
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "cat hung — stdin is not wired to /dev/null"
    );
    // cat with null stdin reads EOF immediately and exits 0. The
    // tool's own empty-output branch kicks in and returns "(no
    // output)" — that's the correct outcome here (not a
    // regression, cat really did produce no stdout).
    let s = result.expect("cat should exit cleanly with null stdin");
    assert!(
        s.contains("(no output)") || s.is_empty(),
        "expected empty output from cat </dev/null, got {s:?}"
    );
}
