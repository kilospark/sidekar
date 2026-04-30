//! `ProcessManager`: the session registry.
//!
//! Owns every live [`UnifiedExecProcess`] and exposes the actions
//! the tool dispatcher speaks:
//!   - `spawn` a new session, get its id
//!   - `write_stdin` + yield on an existing session
//!   - `poll` (= `write_stdin` with empty bytes) on an existing session
//!   - `kill` an existing session (SIGHUP, then the reader thread reaps)
//!   - `list` all live sessions
//!   - `terminate_all` for `/new` and Drop
//!
//! State is a `HashMap<i32, Arc<UnifiedExecProcess>>` behind an
//! `async_mutex::Mutex`. The mutex is held only for the registry
//! operation itself — the session's own `yield_until` runs WITHOUT
//! this lock held, so long yields don't block other sessions.
//!
//! Id allocation is monotonic (`AtomicI32`, starts at 1). No recycling:
//! every id ever handed out is unique within a ProcessManager's
//! lifetime. Wrapping at i32::MAX is hypothetical — the REPL will
//! restart first.

use anyhow::{Result, anyhow, bail};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

use super::process::{SpawnOptions, UnifiedExecProcess, YieldResult};

/// Cap on concurrently live sessions. Codex uses 64; sidekar is a
/// single-user REPL where 32 is plenty. When full, `spawn` returns
/// an error naming the cap so the model can kill or wait.
pub const MAX_SESSIONS: usize = 32;

/// Default yield time for spawn (model can override). Mirrors codex.
pub const DEFAULT_SPAWN_YIELD_MS: u64 = 10_000;

/// Default yield for write_stdin with non-empty input. Keep short —
/// the model is steering interactive programs and wants fast
/// round-trip.
pub const DEFAULT_WRITE_YIELD_MS: u64 = 250;

/// Default yield for poll (write_stdin with empty input). Longer —
/// the model is explicitly asking to wait for activity.
pub const DEFAULT_POLL_YIELD_MS: u64 = 5_000;

/// Floor and ceiling for yield values. Users/models must fit in
/// this range; out-of-range values are clamped silently. Matches
/// codex's `clamp_yield_time`.
pub const MIN_YIELD_MS: u64 = 250;
pub const MAX_YIELD_MS: u64 = 30_000;

pub fn clamp_yield_ms(v: u64) -> u64 {
    v.clamp(MIN_YIELD_MS, MAX_YIELD_MS)
}

/// Per-session bookkeeping layered on top of the raw process. The
/// cursor lives here because `ProcessManager` mediates all output
/// reads — the caller doesn't track position, we do.
struct SessionEntry {
    process: Arc<UnifiedExecProcess>,
    /// Logical position into the buffer. Each yield advances this
    /// to `YieldResult::{Output,Exited,Yielded}.position_after`.
    /// On Cancelled the cursor is NOT advanced, so the next call
    /// sees the same pending bytes.
    cursor: u64,
}

/// Snapshot of a live session for the `list` tool output.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_id: i32,
    pub command: String,
    /// Unix timestamp (seconds) when the session was spawned.
    pub started_at_unix: u64,
    /// Seconds elapsed since spawn.
    pub age_seconds: f64,
    /// Approximate bytes currently captured (post head-tail). Useful
    /// for the model to decide whether to drain a noisy session.
    pub buffer_bytes: u64,
    /// Whether the child is still running (exit_code not yet set).
    pub alive: bool,
}

/// Public manager handle. Clone-cheap (`Arc`).
#[derive(Clone)]
pub struct ProcessManager {
    inner: Arc<Inner>,
}

struct Inner {
    sessions: Mutex<HashMap<i32, SessionEntry>>,
    next_id: AtomicI32,
    /// Running count of every call into the manager. Logged on REPL
    /// exit to confirm usage. If this stays ~0 over a release, rip
    /// the tool out rather than carrying the token tax for every
    /// user.
    usage_count: AtomicI32,
}

/// Result of a spawn/poll/write call — the JSON output shape
/// expressed as Rust. The tool dispatcher converts this to the
/// documented JSON shape in context/unified-exec.md §Output shape.
#[derive(Debug)]
pub struct ExecOutput {
    pub output: Vec<u8>,
    pub wall_time_ms: u64,
    /// Present iff session is still alive.
    pub session_id: Option<i32>,
    /// Present iff the child exited during this call.
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                sessions: Mutex::new(HashMap::new()),
                next_id: AtomicI32::new(1),
                usage_count: AtomicI32::new(0),
            }),
        }
    }

    /// Returns total calls into the manager. Used for usage
    /// reporting on REPL exit.
    pub fn usage_count(&self) -> i32 {
        self.inner.usage_count.load(Ordering::Relaxed)
    }

    /// Returns current number of live sessions.
    pub async fn session_count(&self) -> usize {
        self.inner.sessions.lock().await.len()
    }

    /// Reap sessions whose child has exited. Called opportunistically
    /// before each spawn to keep the store from filling with zombies.
    /// Does NOT kill anything — only removes entries already dead.
    async fn reap_exited(&self) {
        let mut sessions = self.inner.sessions.lock().await;
        let mut to_remove = Vec::new();
        for (&id, entry) in sessions.iter() {
            // Try to take the state lock briefly. We must not hold
            // the registry lock through long operations, so read
            // the exit flag and move on.
            if entry
                .process
                .state
                .try_lock()
                .map(|st| st.exit_code.is_some())
                .unwrap_or(false)
            {
                to_remove.push(id);
            }
        }
        for id in to_remove {
            sessions.remove(&id);
        }
    }

    /// Spawn a new session. Returns `(session_id, initial_yield)`.
    pub async fn spawn(
        &self,
        opts: SpawnOptions,
        yield_ms: u64,
        cancel: Option<&Arc<AtomicBool>>,
    ) -> Result<ExecOutput> {
        self.inner.usage_count.fetch_add(1, Ordering::Relaxed);
        self.reap_exited().await;

        {
            let sessions = self.inner.sessions.lock().await;
            if sessions.len() >= MAX_SESSIONS {
                bail!(
                    "too many live exec sessions ({}/{}); kill one or wait for completion",
                    sessions.len(),
                    MAX_SESSIONS
                );
            }
        }

        let proc = UnifiedExecProcess::spawn(opts)?;
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);

        // Wait up to `yield_ms` for initial output or exit. We do
        // this BEFORE inserting into the store so that a command
        // which fails immediately (e.g. shell not found, syntax
        // error) reports Exited and doesn't leave a zombie entry.
        //
        // If the yield says Cancelled, we still insert so the model
        // can revisit. Matches the rule that cancel ≠ kill.
        let deadline = Instant::now() + Duration::from_millis(clamp_yield_ms(yield_ms));
        let start = Instant::now();
        let result = proc.yield_until(0, deadline, cancel).await;
        let wall_ms = start.elapsed().as_millis() as u64;

        match result {
            YieldResult::Exited {
                output,
                exit_code,
                signal,
                ..
            } => {
                // Fast-exit path: don't register. No session_id in
                // the output — the model knows there's nothing to
                // come back to.
                Ok(ExecOutput {
                    output,
                    wall_time_ms: wall_ms,
                    session_id: None,
                    exit_code: Some(exit_code),
                    signal,
                })
            }
            YieldResult::Output {
                output,
                position_after,
            } => {
                // Race window: PTY delivers output bytes via the
                // reader thread, and the child's exit status lands
                // on `Child::wait()` a few ms later. It's entirely
                // possible for the yield loop to return Output
                // milliseconds before `exit_code` gets set. For a
                // command that was always going to exit fast
                // (`echo hi`, `ls`, `exit 0`), we don't want to
                // surface a session_id the model will discover is
                // dead on its next poll — that wastes a turn.
                //
                // Short grace: wait up to 100ms for the exit code
                // to appear. If it shows up, promote to Exited
                // and don't register. If not, register as a live
                // session. The grace window is intentionally
                // small — longer waits would penalize genuinely
                // long-running commands that just happened to
                // print their banner quickly.
                let grace_deadline = Instant::now() + Duration::from_millis(100);
                let mut exited = None;
                while Instant::now() < grace_deadline {
                    // Critical: read exit_code AND signal under the
                    // same guard, then drop it BEFORE sleeping or
                    // any later work. An earlier version of this
                    // loop used two separate `proc.state.lock()`
                    // calls back-to-back inside an `if let`, which
                    // rust keeps alive as an `if let`-scoped
                    // temporary — the second lock then self-
                    // deadlocked. Hence the explicit scope here.
                    let snapshot = {
                        let st = proc.state.lock().await;
                        st.exit_code.map(|c| (c, st.signal.clone()))
                    };
                    if let Some(pair) = snapshot {
                        exited = Some(pair);
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }

                match exited {
                    Some((code, signal)) => {
                        // Drain any bytes that arrived in the
                        // grace window too. Cursor is
                        // `position_after` from the first yield.
                        let tail = proc.state.lock().await.buffer.drain_since(position_after);
                        let mut out_bytes = output;
                        out_bytes.extend_from_slice(&tail);
                        Ok(ExecOutput {
                            output: out_bytes,
                            wall_time_ms: start.elapsed().as_millis() as u64,
                            session_id: None,
                            exit_code: Some(code),
                            signal,
                        })
                    }
                    None => {
                        let mut sessions = self.inner.sessions.lock().await;
                        sessions.insert(
                            id,
                            SessionEntry {
                                process: proc,
                                cursor: position_after,
                            },
                        );
                        Ok(ExecOutput {
                            output,
                            wall_time_ms: wall_ms,
                            session_id: Some(id),
                            exit_code: None,
                            signal: None,
                        })
                    }
                }
            }
            YieldResult::Yielded { position_after } => {
                let mut sessions = self.inner.sessions.lock().await;
                sessions.insert(
                    id,
                    SessionEntry {
                        process: proc,
                        cursor: position_after,
                    },
                );
                Ok(ExecOutput {
                    output: Vec::new(),
                    wall_time_ms: wall_ms,
                    session_id: Some(id),
                    exit_code: None,
                    signal: None,
                })
            }
            YieldResult::Cancelled => {
                // Register the session so the model can attach
                // later, then surface Cancelled to the caller.
                let cursor = proc.state.lock().await.buffer.position();
                let mut sessions = self.inner.sessions.lock().await;
                sessions.insert(
                    id,
                    SessionEntry {
                        process: proc,
                        cursor,
                    },
                );
                bail!("cancelled")
            }
        }
    }

    /// Write bytes (may be empty for pure poll), then yield.
    pub async fn write_stdin(
        &self,
        session_id: i32,
        bytes: &[u8],
        yield_ms: u64,
        cancel: Option<&Arc<AtomicBool>>,
    ) -> Result<ExecOutput> {
        self.inner.usage_count.fetch_add(1, Ordering::Relaxed);

        // Clone the Arc out of the registry; release the lock
        // before we yield. Holding the registry lock through a
        // long yield would serialize ALL session operations.
        let process: Arc<UnifiedExecProcess>;
        let cursor: u64;
        {
            let sessions = self.inner.sessions.lock().await;
            let entry = sessions
                .get(&session_id)
                .ok_or_else(|| anyhow!("unknown session_id {session_id}"))?;
            process = entry.process.clone();
            cursor = entry.cursor;
        }

        if !bytes.is_empty() {
            // Writer is synchronous; we run it inline because
            // write+flush on a pty master is very fast.
            process
                .write_stdin(bytes)
                .map_err(|e| anyhow!("write_stdin: {e}"))?;
            // Give the child a brief moment to react — codex does
            // the same 100ms sleep for the same reason: increases
            // the chance we catch its response in this call instead
            // of the next poll.
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let deadline = Instant::now() + Duration::from_millis(clamp_yield_ms(yield_ms));
        let start = Instant::now();
        let result = process.yield_until(cursor, deadline, cancel).await;
        let wall_ms = start.elapsed().as_millis() as u64;

        let (output, exit_code, signal, still_alive, new_cursor) = match result {
            YieldResult::Output {
                output,
                position_after,
            } => (output, None, None, true, position_after),
            YieldResult::Exited {
                output,
                position_after,
                exit_code,
                signal,
            } => (output, Some(exit_code), signal, false, position_after),
            YieldResult::Yielded { position_after } => {
                (Vec::new(), None, None, true, position_after)
            }
            YieldResult::Cancelled => {
                // Don't advance cursor on cancel. Don't remove from
                // registry. Bubble up.
                bail!("cancelled");
            }
        };

        // Update cursor / remove if exited.
        {
            let mut sessions = self.inner.sessions.lock().await;
            if still_alive {
                if let Some(entry) = sessions.get_mut(&session_id) {
                    entry.cursor = new_cursor;
                }
            } else {
                sessions.remove(&session_id);
            }
        }

        Ok(ExecOutput {
            output,
            wall_time_ms: wall_ms,
            session_id: still_alive.then_some(session_id),
            exit_code,
            signal,
        })
    }

    /// Kill a session. Sends SIGHUP via portable-pty; the reader
    /// thread will observe EOF and reap the exit status. Removes
    /// the entry from the registry synchronously — even if the
    /// signal doesn't immediately kill, the session is no longer
    /// addressable.
    pub async fn kill(&self, session_id: i32) -> Result<ExecOutput> {
        self.inner.usage_count.fetch_add(1, Ordering::Relaxed);

        let entry = {
            let mut sessions = self.inner.sessions.lock().await;
            sessions
                .remove(&session_id)
                .ok_or_else(|| anyhow!("unknown session_id {session_id}"))?
        };

        entry.process.terminate();

        // Give the child up to 500ms to die and get reaped, so we
        // can return the exit code in the same call. Not a hard
        // guarantee — stubborn processes may outlive this window.
        let deadline = Instant::now() + Duration::from_millis(500);
        let start = Instant::now();
        let result = entry
            .process
            .yield_until(entry.cursor, deadline, None)
            .await;
        let wall_ms = start.elapsed().as_millis() as u64;

        match result {
            YieldResult::Exited {
                output,
                exit_code,
                signal,
                ..
            } => Ok(ExecOutput {
                output,
                wall_time_ms: wall_ms,
                session_id: None,
                exit_code: Some(exit_code),
                signal,
            }),
            // Still running after SIGHUP + 500ms. Surface what we
            // have; the reaper thread will eventually clean up even
            // though we've dropped our entry. In practice the Arc
            // holds the reader thread alive until exit.
            YieldResult::Output { output, .. } => Ok(ExecOutput {
                output,
                wall_time_ms: wall_ms,
                session_id: None,
                exit_code: None,
                signal: None,
            }),
            YieldResult::Yielded { .. } => Ok(ExecOutput {
                output: Vec::new(),
                wall_time_ms: wall_ms,
                session_id: None,
                exit_code: None,
                signal: None,
            }),
            YieldResult::Cancelled => Ok(ExecOutput {
                output: Vec::new(),
                wall_time_ms: wall_ms,
                session_id: None,
                exit_code: None,
                signal: None,
            }),
        }
    }

    /// Enumerate live sessions. Order is unspecified (hash map).
    pub async fn list(&self) -> Vec<SessionInfo> {
        self.inner.usage_count.fetch_add(1, Ordering::Relaxed);

        let sessions = self.inner.sessions.lock().await;
        let mut out = Vec::with_capacity(sessions.len());
        for (&id, entry) in sessions.iter() {
            let started_at_unix = system_time_seconds(entry.process.started_at);
            let age_seconds = entry.process.started_at.elapsed().as_secs_f64();
            let (alive, buffer_bytes) = match entry.process.state.try_lock() {
                Ok(st) => (st.exit_code.is_none(), st.buffer.total_written()),
                Err(_) => (true, 0), // lock contended; assume alive, unknown bytes
            };
            out.push(SessionInfo {
                session_id: id,
                command: format_command(&entry.process.command),
                started_at_unix,
                age_seconds,
                buffer_bytes,
                alive,
            });
        }
        out
    }

    /// Terminate every live session. Called by `/new` and on REPL
    /// shutdown. Best-effort: signals are sent to all, but we don't
    /// wait for reapers to complete.
    pub async fn terminate_all(&self) {
        let entries: Vec<SessionEntry> = {
            let mut sessions = self.inner.sessions.lock().await;
            sessions.drain().map(|(_, v)| v).collect()
        };
        for entry in entries {
            entry.process.terminate();
        }
    }
}

/// Approximate wall-clock unix seconds corresponding to the given
/// Instant. Instant is a monotonic clock with no epoch, so we
/// bridge via SystemTime::now() - Instant::now() + Instant.
fn system_time_seconds(at: Instant) -> u64 {
    let now_system = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let now_instant = Instant::now();
    if at <= now_instant {
        let delta = now_instant - at;
        now_system.as_secs().saturating_sub(delta.as_secs())
    } else {
        // Spawned in the future? Shouldn't happen; clamp to now.
        now_system.as_secs()
    }
}

/// Join a command vec for display. `["bash", "-lc", "npm run dev"]`
/// reads most naturally as `bash -lc 'npm run dev'`. We just
/// re-quote the last arg if it contains whitespace; everything
/// before is passed through verbatim.
fn format_command(cmd: &[String]) -> String {
    if cmd.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for (i, arg) in cmd.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if arg.chars().any(|c| c.is_whitespace()) {
            out.push('\'');
            out.push_str(arg);
            out.push('\'');
        } else {
            out.push_str(arg);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    //! Tests for ProcessManager.
    //!
    //! These are real-subprocess integration tests — same model as
    //! process.rs's tests. They verify:
    //!
    //!   1. spawn + immediate Exited result does not register a
    //!      session.
    //!   2. spawn of a long-running command registers and returns
    //!      a session_id.
    //!   3. write_stdin round-trips through cat.
    //!   4. poll (empty write) returns newly-arrived output.
    //!   5. kill removes the entry and returns exit info.
    //!   6. list enumerates correctly.
    //!   7. MAX_SESSIONS cap rejects the (MAX+1)th spawn.
    //!   8. terminate_all drains everything.
    //!   9. usage_count increments on every call.
    //!  10. Cursor advancement across write_stdin + poll doesn't
    //!      double-report the same bytes.

    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;
    use tokio::time::sleep;

    fn opts(cmd: &str) -> SpawnOptions {
        SpawnOptions {
            cmd: cmd.into(),
            shell: None,
            workdir: None,
            tty: true,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fast_exit_does_not_register_session() {
        let mgr = ProcessManager::new();
        let out = mgr
            .spawn(opts("echo hi; exit 0"), 3000, None)
            .await
            .unwrap();
        assert!(out.session_id.is_none(), "fast-exit must not register");
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(mgr.session_count().await, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn long_running_registers_session_id() {
        let mgr = ProcessManager::new();
        let out = mgr.spawn(opts("sleep 30"), 300, None).await.unwrap();
        let sid = out
            .session_id
            .expect("long-running command must return session_id");
        assert!(sid >= 1);
        assert_eq!(mgr.session_count().await, 1);

        // Cleanup.
        mgr.kill(sid).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn write_stdin_round_trips_through_cat() {
        let mgr = ProcessManager::new();
        let out = mgr.spawn(opts("cat"), 200, None).await.unwrap();
        let sid = out.session_id.expect("cat should still be running");

        // Send a line. PTY echoes it back.
        let out2 = mgr.write_stdin(sid, b"hello\n", 500, None).await.unwrap();
        let s = String::from_utf8_lossy(&out2.output);
        assert!(s.contains("hello"), "cat should echo; got: {s:?}");

        // Send EOF → cat exits.
        let out3 = mgr.write_stdin(sid, &[0x04], 2000, None).await.unwrap();
        assert_eq!(out3.exit_code, Some(0));
        assert_eq!(
            mgr.session_count().await,
            0,
            "exited session must be reaped"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn poll_returns_newly_arrived_output() {
        // Shell writes, sleeps, writes again. First spawn catches
        // the first write; poll with empty bytes catches the
        // second.
        let mgr = ProcessManager::new();
        let out = mgr
            .spawn(
                opts("printf one\\\\n; sleep 0.5; printf two\\\\n; sleep 30"),
                300,
                None,
            )
            .await
            .unwrap();
        let sid = out.session_id.unwrap();
        let first = String::from_utf8_lossy(&out.output).to_string();

        // First yield may have returned right after 'one' or
        // before anything at all if spawn was fast enough to
        // timeout on 300ms. Poll to grab the rest.
        let out2 = mgr.write_stdin(sid, b"", 2000, None).await.unwrap();
        let second = String::from_utf8_lossy(&out2.output).to_string();

        let combined = format!("{first}{second}");
        assert!(
            combined.contains("two"),
            "must see 'two' across calls; got: {combined:?}"
        );

        mgr.kill(sid).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cursor_does_not_double_report_bytes() {
        // Regression guard: two successive write_stdin calls with
        // empty input should not see the same bytes twice.
        let mgr = ProcessManager::new();
        let out = mgr
            .spawn(opts("printf alpha\\\\n; sleep 30"), 300, None)
            .await
            .unwrap();
        let sid = out.session_id.unwrap();
        let first_seen = String::from_utf8_lossy(&out.output).contains("alpha");

        // Drain anything pending with a quick poll.
        let out2 = mgr.write_stdin(sid, b"", 300, None).await.unwrap();
        let second_seen = String::from_utf8_lossy(&out2.output).contains("alpha");

        // Second poll: should see nothing (process is sleeping).
        let out3 = mgr.write_stdin(sid, b"", 300, None).await.unwrap();
        let third_seen = String::from_utf8_lossy(&out3.output).contains("alpha");

        // 'alpha' must appear in exactly one of first+second, and
        // must NOT appear in third.
        assert!(
            first_seen || second_seen,
            "alpha should appear in spawn or first poll"
        );
        assert!(!third_seen, "alpha must not repeat on subsequent poll");

        mgr.kill(sid).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn kill_removes_entry_and_returns_exit_info() {
        let mgr = ProcessManager::new();
        let out = mgr.spawn(opts("sleep 60"), 200, None).await.unwrap();
        let sid = out.session_id.unwrap();
        assert_eq!(mgr.session_count().await, 1);

        let killed = mgr.kill(sid).await.unwrap();
        assert!(killed.session_id.is_none());
        assert_eq!(mgr.session_count().await, 0);
        // kill returns exit info when the child reaps in time;
        // with SIGHUP + 500ms window, sleep should be gone.
        // (We don't assert a specific code because signal-killed
        // processes can surface in platform-specific ways.)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn list_enumerates_all_live_sessions() {
        let mgr = ProcessManager::new();
        let mut ids = Vec::new();
        for _ in 0..3 {
            let out = mgr.spawn(opts("sleep 30"), 200, None).await.unwrap();
            ids.push(out.session_id.unwrap());
        }

        let listed = mgr.list().await;
        assert_eq!(listed.len(), 3);
        for info in &listed {
            assert!(info.alive);
            assert!(info.command.contains("sleep 30"));
            assert!(info.age_seconds >= 0.0);
            assert!(ids.contains(&info.session_id));
        }

        mgr.terminate_all().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn max_sessions_cap_rejects_overflow() {
        let mgr = ProcessManager::new();
        // Fill to MAX.
        let mut sids = Vec::new();
        for _ in 0..MAX_SESSIONS {
            let out = mgr.spawn(opts("sleep 60"), 200, None).await.unwrap();
            sids.push(out.session_id.unwrap());
        }

        // The next one must fail.
        let err = mgr
            .spawn(opts("sleep 60"), 200, None)
            .await
            .expect_err("cap should reject");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("too many live exec sessions"),
            "error should mention cap; got: {msg}"
        );

        mgr.terminate_all().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn terminate_all_drains_registry() {
        let mgr = ProcessManager::new();
        for _ in 0..4 {
            mgr.spawn(opts("sleep 60"), 200, None).await.unwrap();
        }
        assert_eq!(mgr.session_count().await, 4);

        mgr.terminate_all().await;
        assert_eq!(mgr.session_count().await, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn usage_count_increments_across_calls() {
        let mgr = ProcessManager::new();
        assert_eq!(mgr.usage_count(), 0);

        let out = mgr.spawn(opts("sleep 30"), 200, None).await.unwrap();
        let sid = out.session_id.unwrap();
        assert_eq!(mgr.usage_count(), 1);

        mgr.write_stdin(sid, b"", 200, None).await.unwrap();
        assert_eq!(mgr.usage_count(), 2);

        mgr.list().await;
        assert_eq!(mgr.usage_count(), 3);

        mgr.kill(sid).await.unwrap();
        assert_eq!(mgr.usage_count(), 4);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_during_spawn_preserves_session() {
        // A cancelled spawn must still register the session so the
        // model can revisit. The invariant: cancel ≠ kill.
        let mgr = ProcessManager::new();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(150)).await;
            cancel_clone.store(true, Ordering::Relaxed);
        });

        let err = mgr
            .spawn(opts("sleep 30"), 5000, Some(&cancel))
            .await
            .expect_err("cancel during spawn should bail");
        assert!(format!("{err:#}").contains("cancelled"));

        // The session was registered despite cancellation.
        let listed = mgr.list().await;
        assert_eq!(listed.len(), 1, "cancelled spawn must leave session alive");

        mgr.terminate_all().await;
    }
}
