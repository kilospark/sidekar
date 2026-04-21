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
