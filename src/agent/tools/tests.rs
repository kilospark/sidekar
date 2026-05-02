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
    let pidfile = tmpdir.join(format!("sidekar_cancel_test_{}.pid", std::process::id()));
    let pidfile_str = pidfile.to_string_lossy().to_string();
    // Ensure stale file doesn't confuse the test.
    let _ = std::fs::remove_file(&pidfile);

    // bash spawns a child `sleep 30` in its own subprocess, writes the
    // grandchild's pid, then waits so bash itself is still alive when we
    // cancel. Use `exec` in a subshell so `$!` points to the real sleep
    // pid rather than an intermediate subshell wrapper.
    let command = format!("( exec sleep 30 ) & echo $! > {pidfile_str}; wait");

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

    let pid_str =
        std::fs::read_to_string(&pidfile).expect("pid file should have been written by bash");
    let pid: i32 = pid_str
        .trim()
        .parse()
        .expect("pid file contents are a number");
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
    let result = execute("bash", &json!({ "command": "cat", "timeout": 3 }), None).await;
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

// -----------------------------------------------------------------
// ExecSession dispatcher tests (M4)
//
// These validate the argument-shape parsing and action routing in
// `exec_exec_session`. They use real subprocesses (cheap on unix)
// but focus on the handler's behavior rather than re-testing
// ProcessManager's internals (covered by manager.rs's suite).
//
// Notes:
//   - The dispatcher uses a process-wide OnceLock-backed
//     ProcessManager. Tests can interact with state from earlier
//     tests, so each test spawns its own session and cleans up
//     (via /kill) at the end.
//   - gated on cfg(unix) just like the handler itself.
// -----------------------------------------------------------------

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_session_spawn_fast_exit_returns_no_session_id() {
    let raw = execute("ExecSession", &json!({ "cmd": "echo hello; exit 0" }), None)
        .await
        .expect("spawn ok");
    // Result is JSON-encoded.
    let v: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
    assert!(
        v.get("session_id").is_none(),
        "fast-exit should not return session_id"
    );
    assert_eq!(v["exit_code"], 0);
    let output = v["output"].as_str().unwrap();
    assert!(
        output.contains("hello"),
        "output must include echo; got: {output}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_session_spawn_then_kill_round_trip() {
    // Spawn a long-running session, verify session_id, then kill.
    let raw = execute(
        "ExecSession",
        &json!({ "cmd": "sleep 60", "yield_time_ms": 300 }),
        None,
    )
    .await
    .expect("spawn ok");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let sid = v["session_id"].as_i64().expect("session_id present");

    // Kill via action.
    let raw2 = execute(
        "ExecSession",
        &json!({ "session_id": sid, "action": "kill" }),
        None,
    )
    .await
    .expect("kill ok");
    let v2: serde_json::Value = serde_json::from_str(&raw2).unwrap();
    assert!(v2.get("session_id").is_none(), "kill must clear session_id");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_session_write_sends_stdin_and_polls() {
    // Spawn `cat`, write a line, expect echo in response.
    let raw = execute(
        "ExecSession",
        &json!({ "cmd": "cat", "yield_time_ms": 300 }),
        None,
    )
    .await
    .expect("spawn ok");
    let sid = serde_json::from_str::<serde_json::Value>(&raw).unwrap()["session_id"]
        .as_i64()
        .expect("session_id present");

    let raw2 = execute(
        "ExecSession",
        &json!({
            "session_id": sid,
            "action": "write",
            "stdin": "echo_me\n",
            "yield_time_ms": 500,
        }),
        None,
    )
    .await
    .expect("write ok");
    let v2: serde_json::Value = serde_json::from_str(&raw2).unwrap();
    let out = v2["output"].as_str().unwrap();
    assert!(out.contains("echo_me"), "cat should echo; got: {out}");

    // EOF → cat exits.
    let _ = execute(
        "ExecSession",
        &json!({
            "session_id": sid,
            "action": "write",
            "stdin": "\u{0004}",
            "yield_time_ms": 2000,
        }),
        None,
    )
    .await
    .expect("eof ok");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_session_list_includes_live_session() {
    // Spawn, list, find it, kill.
    let raw = execute(
        "ExecSession",
        &json!({ "cmd": "sleep 60", "yield_time_ms": 300 }),
        None,
    )
    .await
    .unwrap();
    let sid = serde_json::from_str::<serde_json::Value>(&raw).unwrap()["session_id"]
        .as_i64()
        .unwrap();

    let listed = execute("ExecSession", &json!({ "action": "list" }), None)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&listed).unwrap();
    let arr = v["sessions"].as_array().expect("array");
    let found = arr.iter().any(|s| s["session_id"].as_i64() == Some(sid));
    assert!(found, "list must include our session");

    let _ = execute(
        "ExecSession",
        &json!({ "session_id": sid, "action": "kill" }),
        None,
    )
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn exec_session_rejects_mutually_exclusive_cmd_and_session_id() {
    let err = execute(
        "ExecSession",
        &json!({ "cmd": "echo hi", "session_id": 1 }),
        None,
    )
    .await
    .expect_err("conflict should error");
    let s = format!("{err:#}");
    assert!(
        s.contains("mutually exclusive"),
        "error should call out the conflict; got: {s}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn exec_session_rejects_write_without_stdin() {
    let err = execute(
        "ExecSession",
        &json!({ "session_id": 99999, "action": "write" }),
        None,
    )
    .await
    .expect_err("missing stdin should error");
    assert!(format!("{err:#}").contains("stdin"));
}

#[cfg(unix)]
#[tokio::test]
async fn exec_session_rejects_unknown_action() {
    let err = execute(
        "ExecSession",
        &json!({ "session_id": 1, "action": "bogus" }),
        None,
    )
    .await
    .expect_err("bad action should error");
    let s = format!("{err:#}");
    assert!(
        s.contains("unknown action"),
        "error should name the unknown action; got: {s}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn exec_session_requires_cmd_or_session_id() {
    let err = execute("ExecSession", &json!({}), None)
        .await
        .expect_err("empty args should error");
    let s = format!("{err:#}");
    assert!(s.contains("cmd") && s.contains("session_id"));
}

#[cfg(unix)]
#[tokio::test]
async fn exec_session_rejects_action_on_spawn() {
    let err = execute(
        "ExecSession",
        &json!({ "cmd": "echo hi", "action": "poll" }),
        None,
    )
    .await
    .expect_err("action with cmd should error");
    assert!(format!("{err:#}").contains("'action'"));
}

/// Regression guard: ExecSession must appear in the tool list on
/// unix. A stray cfg(unix) gate mismatch would silently drop the
/// tool from model-facing definitions() without failing any other
/// test. Verify presence explicitly.
#[cfg(unix)]
#[test]
fn exec_session_is_registered_in_definitions() {
    let defs = super::definitions();
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert!(
        names.contains(&"ExecSession"),
        "ExecSession must be in the tool list on unix; got: {names:?}"
    );
    // Also sanity-check the schema doesn't have `required` — we
    // validate inside the handler, not at schema level. A stray
    // required list would reject legitimate argument shapes.
    let es = defs
        .iter()
        .find(|d| d.name == "ExecSession")
        .expect("present");
    let schema = &es.input_schema;
    assert!(
        schema.get("required").is_none(),
        "ExecSession schema must not pin required fields; handler validates: {schema}"
    );
}

// -----------------------------------------------------------------
// Edit tool
// -----------------------------------------------------------------

fn edit_test_path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "sidekar_edit_{}_{}_{}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

#[tokio::test]
async fn edit_duplicate_match_lists_line_numbers() {
    let path = edit_test_path("dup");
    std::fs::write(&path, "a\nfoo\nb\nfoo\nc\n").unwrap();
    let err = execute(
        "edit",
        &json!({
            "path": path.to_str().unwrap(),
            "old_string": "foo",
            "new_string": "bar",
        }),
        None,
    )
    .await
    .expect_err("ambiguous match");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("2") && msg.contains("4"),
        "expected line numbers in error: {msg}"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn edit_normalizes_lf_snippet_against_crlf_file() {
    let path = edit_test_path("crlf");
    std::fs::write(&path, "line1\r\nTARGET\r\nline3\r\n").unwrap();
    let out = execute(
        "edit",
        &json!({
            "path": path.to_str().unwrap(),
            "old_string": "TARGET\n",
            "new_string": "DONE\n",
        }),
        None,
    )
    .await
    .expect("LF needle should match CRLF span");
    assert!(out.contains("Replaced"), "{out}");
    let body = std::fs::read_to_string(&path).expect("read back");
    assert!(body.contains("DONE"), "{body:?}");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn edit_rejects_empty_old_string() {
    let path = edit_test_path("empty");
    std::fs::write(&path, "x").unwrap();
    let err = execute(
        "edit",
        &json!({
            "path": path.to_str().unwrap(),
            "old_string": "",
            "new_string": "y",
        }),
        None,
    )
    .await
    .expect_err("empty old_string");
    assert!(
        format!("{err:#}").contains("must not be empty"),
        "{err:#}"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn edit_not_found_suggests_trim_when_trim_matches() {
    let path = edit_test_path("trimhint");
    std::fs::write(&path, "hello\n").unwrap();
    let err = execute(
        "edit",
        &json!({
            "path": path.to_str().unwrap(),
            "old_string": " hello",
            "new_string": "z",
        }),
        None,
    )
    .await
    .expect_err("exact mismatch");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("whitespace") || msg.contains("trimmed"),
        "{msg}"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn edit_occurrence_index_targets_nth_match() {
    let path = edit_test_path("nth");
    std::fs::write(&path, "foo\nfoo\nfoo\n").unwrap();
    let out = execute(
        "edit",
        &json!({
            "path": path.to_str().unwrap(),
            "old_string": "foo",
            "new_string": "BAR",
            "occurrence_index": 2,
        }),
        None,
    )
    .await
    .expect("occurrence_index edit");
    assert!(out.contains("Replaced"), "{out}");
    let body = std::fs::read_to_string(&path).unwrap();
    assert_eq!(body, "foo\nBAR\nfoo\n");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn edit_patch_mode_applies_unified_diff() {
    let path = edit_test_path("patch");
    std::fs::write(&path, "alpha\nbeta\n").unwrap();
    let patch = format!(
        "\
--- a/x.txt
+++ b/x.txt
@@ -1,2 +1,2 @@
 alpha
-beta
+gamma
",
    );
    let out = execute(
        "edit",
        &json!({
            "path": path.to_str().unwrap(),
            "patch": patch,
        }),
        None,
    )
    .await
    .expect("patch edit");
    assert!(out.contains("unified diff"), "{out}");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("gamma"), "{body:?}");
    assert!(!body.contains("beta"), "{body:?}");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn edit_patch_mode_rejects_extra_keys() {
    let path = edit_test_path("patchbad");
    std::fs::write(&path, "x\n").unwrap();
    let err = execute(
        "edit",
        &json!({
            "path": path.to_str().unwrap(),
            "patch": "--- a/x\n+++ b/x\n@@ -1,1 +1,1 @@\n-x\n+y\n",
            "old_string": "x",
        }),
        None,
    )
    .await
    .expect_err("patch + old_string");
    assert!(
        format!("{err:#}").contains("only `path` and `patch`"),
        "{err:#}"
    );
    let _ = std::fs::remove_file(&path);
}
