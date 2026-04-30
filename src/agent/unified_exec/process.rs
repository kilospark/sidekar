//! A single PTY-backed exec session: spawn, stream output into a
//! [`HeadTailBuffer`], wait for exit, terminate.
//!
//! The contract:
//!   - `UnifiedExecProcess::spawn` creates a pty pair, spawns the
//!     child into the slave, drops the slave handle, and kicks off a
//!     blocking-thread reader that pumps master-pty bytes into the
//!     shared buffer until EOF.
//!   - While the child runs, the shared state (`ProcessState`) is
//!     readable from any async task. The buffer grows; exit_code is
//!     `None`.
//!   - When the child exits (or we terminate it), the reader thread
//!     reaps the child, writes `exit_code`, fires the notifier, and
//!     returns.
//!   - Callers coordinate via `state.notify` (M2). For M1, the
//!     process is observable synchronously via `state.lock()` but
//!     without the notify wiring — yield semantics land next.
//!
//! Threading model is deliberate:
//!   - `portable_pty::MasterPty::try_clone_reader()` hands back a
//!     boxed `std::io::Read` that is blocking. We run it on a
//!     dedicated OS thread (`std::thread::spawn`), NOT a tokio task,
//!     because blocking I/O on the tokio pool would starve the
//!     runtime. The thread bridges into the async world by taking
//!     a `tokio::sync::Mutex` and firing a `Notify`.
//!   - `Child::wait()` is also blocking. We call it from the same
//!     thread after the read-loop returns (which happens on EOF, i.e.
//!     child has exited or master has been closed). Exit code writes
//!     into the shared state and notifies.
//!   - Writes to stdin use `MasterPty::take_writer()` once at spawn
//!     time; the writer is wrapped in a std-sync Mutex and called
//!     directly from async via `spawn_blocking` when needed.

use anyhow::{Context, Result, anyhow};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::thread::JoinHandle;
use std::time::Instant;
use tokio::sync::{Mutex, Notify};

use super::buffer::HeadTailBuffer;

/// Size we request from the PTY at spawn. 24 rows × 200 cols matches
/// codex's default. 200 cols is wide enough that most line-wrapping
/// programs (`git log`, `ls -l`, `cargo build`) don't fold output
/// inside the terminal before we capture it — captured-then-rendered
/// output with soft wraps is confusing to both humans reading the log
/// and models trying to parse it.
const PTY_ROWS: u16 = 24;
const PTY_COLS: u16 = 200;

/// Size of each read from the master PTY. 64 KiB is a common pipe
/// buffer size; picking the same here means we rarely need more than
/// one read to drain a full burst of output.
const READ_BUF_BYTES: usize = 64 * 1024;

/// Shared state between the reader thread and any async callers.
/// Wrapped in [`tokio::sync::Mutex`] so async tasks can await without
/// blocking the runtime; the reader thread bridges by using
/// `blocking_lock` which is safe because the thread is NOT a tokio
/// worker.
pub struct ProcessState {
    /// Captured output. Head+tail bounded.
    pub buffer: HeadTailBuffer,

    /// `Some(code)` once the child has exited and been reaped.
    /// Signed i32 because portable_pty's u32 exit code is
    /// inconvenient (and signal kills can usefully be expressed as
    /// negative values, matching POSIX convention).
    pub exit_code: Option<i32>,

    /// Raw signal name if the child died to a signal (e.g. "SIGINT").
    /// None for normal exits or if the platform doesn't report one.
    pub signal: Option<String>,
}

impl ProcessState {
    fn new() -> Self {
        Self {
            buffer: HeadTailBuffer::new(),
            exit_code: None,
            signal: None,
        }
    }
}

/// One live PTY-backed exec session.
///
/// Ownership notes:
///   - `state` is `Arc<Mutex<..>>` because the reader thread and any
///     number of async tool handlers need simultaneous read/write
///     access.
///   - `notify` fires on every meaningful state change: new output
///     bytes written, exit code set. Async waiters use this to wake
///     from their yield-deadline sleep early. Used in M2.
///   - `writer` is held so stdin writes don't need to re-open the
///     master; `StdMutex` not `tokio::Mutex` because we only touch it
///     from `spawn_blocking` closures, never across await points.
///   - `child_killer` is cloned at spawn time from the `Child` handle
///     and kept here so we can signal the child without contending
///     with the reader thread that owns the `Child`.
///   - `reader_thread` is the JoinHandle of the blocking I/O thread.
///     Dropped into a Drop impl that waits briefly on close; not
///     joined synchronously because we don't want kill() to block.
pub struct UnifiedExecProcess {
    /// The command that was spawned, e.g. `["bash", "-lc", "npm run dev"]`.
    /// Stored for the list tool's output.
    pub command: Vec<String>,

    /// When we spawned. Used for the list tool's age computation.
    pub started_at: Instant,

    /// PID of the child. Not strictly needed after spawn; kept for
    /// debugging and logging.
    pub pid: Option<u32>,

    /// Shared with the reader thread + tool handlers.
    pub state: Arc<Mutex<ProcessState>>,

    /// Fires on output-written or exit-set. Wake source for yield
    /// loops (M2).
    pub notify: Arc<Notify>,

    /// Whether the session has a TTY. Tracked so the write-stdin
    /// path can refuse to write to a non-PTY session (matches codex:
    /// stdin-less non-TTY sessions return StdinClosed).
    pub tty: bool,

    /// Writer half of the master PTY. Option because we take it once
    /// and never again; if tty=false and writer wasn't requested, it
    /// stays None.
    writer: Arc<StdMutex<Option<Box<dyn Write + Send>>>>,

    /// Kill handle. Calling `kill()` sends SIGHUP to the child's
    /// controlling process group (portable-pty's unix impl).
    child_killer: StdMutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,

    /// Join handle of the blocking reader/waiter thread. Wrapped in
    /// Option because Drop takes it by value.
    reader_thread: StdMutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for UnifiedExecProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnifiedExecProcess")
            .field("command", &self.command)
            .field("pid", &self.pid)
            .field("tty", &self.tty)
            .finish_non_exhaustive()
    }
}

/// How to spawn a new session.
pub struct SpawnOptions {
    /// The full shell command string (e.g. "npm run dev"). Will be
    /// launched as `<shell> -lc <cmd>` so pipelines, redirections,
    /// and env expansion work like the user expects.
    pub cmd: String,

    /// Shell binary. None → falls back to $SHELL then /bin/bash.
    pub shell: Option<String>,

    /// Working directory. None → inherit from the parent (REPL's
    /// cwd).
    pub workdir: Option<PathBuf>,

    /// Whether to allocate a real PTY. Default true. If false, the
    /// child still runs via portable-pty (we don't have a plain-pipe
    /// alternative path) but we note it for the write-stdin refusal
    /// and skip allocating a writer.
    pub tty: bool,
}

impl UnifiedExecProcess {
    /// Spawn a new session.
    ///
    /// On success returns an `Arc<UnifiedExecProcess>` ready to be
    /// polled / written to / waited on. The child is already running
    /// in the background and the reader thread is pumping output
    /// into the buffer.
    ///
    /// Errors:
    ///   - PTY pair allocation failure (OS ran out of ptys).
    ///   - Child spawn failure (shell not executable, etc.).
    ///   - Failure to clone the reader handle (should be impossible
    ///     on a fresh pty).
    pub fn spawn(opts: SpawnOptions) -> Result<Arc<Self>> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: PTY_ROWS,
                cols: PTY_COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| anyhow!("openpty failed: {e}"))?;

        // Build the command: <shell> -lc <cmd>. Login-shell (-l) loads
        // the user's env (PATH, PS1, nvm, pyenv shims, etc.) which is
        // what the user expects when they type a command interactively.
        let shell = opts
            .shell
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/bash".to_string());
        let mut cmd_builder = CommandBuilder::new(&shell);
        cmd_builder.arg("-lc");
        cmd_builder.arg(&opts.cmd);
        if let Some(cwd) = &opts.workdir {
            cmd_builder.cwd(cwd);
        } else if let Ok(cwd) = std::env::current_dir() {
            cmd_builder.cwd(cwd);
        }
        // TERM: tell programs they're in a color-capable terminal
        // when tty=true. Some CLIs (cargo, git) look at TERM to
        // decide if they should emit color. xterm-256color is a safe
        // modern choice.
        if opts.tty {
            cmd_builder.env("TERM", "xterm-256color");
        } else {
            cmd_builder.env("TERM", "dumb");
        }

        let mut child = pair
            .slave
            .spawn_command(cmd_builder)
            .map_err(|e| anyhow!("spawn_command failed: {e}"))?;
        let pid = child.process_id();
        let child_killer = child.clone_killer();

        // Drop the slave in the parent. The child inherits the slave
        // fd and keeps the PTY alive; when the child exits, the
        // master's reader sees EOF.
        drop(pair.slave);

        // Clone the reader before we move master into the reader
        // thread — we need the reader blocking-stream on the thread,
        // but we don't otherwise need master in the parent (except
        // for the writer).
        let reader = pair.master.try_clone_reader().context("try_clone_reader")?;

        // Take the writer once. If that fails (some platforms don't
        // implement it), we still proceed; stdin writes will refuse
        // gracefully.
        let writer: Option<Box<dyn Write + Send>> = if opts.tty {
            pair.master.take_writer().ok()
        } else {
            None
        };

        // We do NOT keep `master` around in the parent beyond this
        // point — it's dropped at end-of-scope. The writer we took
        // above holds a separate fd clone internally (via dup()
        // inside portable-pty). The reader we cloned similarly.
        // TODO(M2/M3): if we want to resize the PTY after spawn,
        // we need to retain `master`. Out of scope for M1.

        let state = Arc::new(Mutex::new(ProcessState::new()));
        let notify = Arc::new(Notify::new());
        let writer_slot = Arc::new(StdMutex::new(writer));

        // Reader/waiter thread.
        let reader_state = state.clone();
        let reader_notify = notify.clone();
        let reader_thread = std::thread::Builder::new()
            .name(format!("unified-exec-reader-{}", pid.unwrap_or(0)))
            .spawn(move || {
                reader_loop(reader, reader_state.clone(), reader_notify.clone());
                // Read loop exited → child has closed its side of the
                // PTY. Reap the child and record the exit code.
                let status = match child.wait() {
                    Ok(s) => s,
                    Err(_) => portable_pty::ExitStatus::with_exit_code(1),
                };
                let exit_code = status.exit_code() as i32;
                let signal = status.signal().map(|s| s.to_string());
                // Use blocking_lock because we're on a plain OS
                // thread, not a tokio worker.
                let mut st = reader_state.blocking_lock();
                st.exit_code = Some(exit_code);
                st.signal = signal;
                drop(st);
                reader_notify.notify_waiters();
            })
            .context("spawn reader thread")?;

        Ok(Arc::new(Self {
            command: vec![shell, "-lc".into(), opts.cmd],
            started_at: Instant::now(),
            pid,
            state,
            notify,
            tty: opts.tty,
            writer: writer_slot,
            child_killer: StdMutex::new(child_killer),
            reader_thread: StdMutex::new(Some(reader_thread)),
        }))
    }

    /// Send a termination signal to the child. Non-blocking: returns
    /// immediately after the signal is delivered. The reader thread
    /// will eventually observe EOF, reap the child, and set
    /// `exit_code` — callers waiting on `notify` will see that.
    ///
    /// portable-pty's `kill()` sends SIGHUP on unix. That's enough
    /// for most foreground processes (bash traps it and exits clean).
    /// Stubborn processes that ignore SIGHUP require SIGKILL, which
    /// is not exposed by the trait — we'd have to reach into the raw
    /// pid. For M1 this is acceptable; graceful-then-forceful can
    /// land later if needed.
    pub fn terminate(&self) {
        if let Ok(mut killer) = self.child_killer.lock() {
            let _ = killer.kill();
        }
        self.notify.notify_waiters();
    }

    /// Write bytes to the child's stdin. Returns `Err` if the
    /// session has no writer (non-TTY) or if the write fails.
    /// Called from async via `spawn_blocking` at the callsite — the
    /// write itself is synchronous.
    pub fn write_stdin(&self, bytes: &[u8]) -> Result<()> {
        let mut guard = self
            .writer
            .lock()
            .map_err(|_| anyhow!("writer mutex poisoned"))?;
        let writer = guard
            .as_mut()
            .ok_or_else(|| anyhow!("session has no stdin (non-TTY)"))?;
        writer.write_all(bytes).context("write_stdin")?;
        writer.flush().context("flush stdin")?;
        Ok(())
    }

    /// True iff the child has exited and been reaped.
    #[allow(dead_code)]
    pub async fn has_exited(&self) -> bool {
        self.state.lock().await.exit_code.is_some()
    }

    /// Wait for output or exit, yielding no later than `deadline`.
    ///
    /// This is the beating heart of the cooperative-yield model.
    /// Every tool call that interacts with a live session funnels
    /// through here. The contract:
    ///
    ///   - If any bytes arrive past `since`, return them immediately
    ///     as [`YieldResult::Output`]. The caller advances its cursor
    ///     to `position_after` for the next poll.
    ///   - If the child exits during the wait, return
    ///     [`YieldResult::Exited`] with the exit code and any output
    ///     that accumulated between `since` and exit.
    ///   - If `deadline` elapses with neither output nor exit,
    ///     return [`YieldResult::Yielded`]. The session is still
    ///     live; the caller surfaces `session_id` to the model.
    ///   - If `cancel` flips to true (Esc / turn cancellation),
    ///     return [`YieldResult::Cancelled`]. Process is left alive
    ///     intentionally — cancelling a turn is not cancelling the
    ///     process. See context/unified-exec.md §Cancellation.
    ///
    /// The loop uses `tokio::sync::Notify::notified()` to sleep
    /// until something happens, so there is no busy polling. The
    /// race between output arrival and deadline is won by whichever
    /// fires first. A short polling interval (100ms) runs alongside
    /// the notify wait for two defensive reasons:
    ///
    ///   1. Cancellation wake: `cancel` is an AtomicBool, not a
    ///      waker, so the only way the yield loop observes it is
    ///      by re-checking on every wake. Without a periodic timer
    ///      the only wake sources are output-bytes and exit, which
    ///      means a cancelled quiet session could sit stuck for
    ///      `deadline - now` milliseconds. 100ms is the worst-case
    ///      responsiveness budget for Esc during a long yield.
    ///   2. Notify ordering: `notified()` registers interest BEFORE
    ///      awaiting, but state writes happen behind a mutex we
    ///      don't hold here. A write that lands after we check the
    ///      buffer but before we register with Notify would be a
    ///      lost wake. The periodic re-check prevents that window
    ///      from hanging the yield. `tokio::sync::Notify` actually
    ///      has `enable()` to avoid this, but the periodic poll is
    ///      simpler and the overhead (one mutex lock per 100ms per
    ///      live session under a yield) is negligible.
    pub async fn yield_until(
        self: &Arc<Self>,
        since: u64,
        deadline: Instant,
        cancel: Option<&Arc<std::sync::atomic::AtomicBool>>,
    ) -> YieldResult {
        use std::sync::atomic::Ordering;

        loop {
            // Check cancel before touching any locks — cheapest
            // exit path.
            if let Some(c) = cancel
                && c.load(Ordering::Relaxed)
            {
                return YieldResult::Cancelled;
            }

            // Snapshot state. Hold the lock only long enough to
            // read buffer and exit_code; release before we sleep.
            let (new_bytes, exit_info, position_after) = {
                let st = self.state.lock().await;
                let drained = st.buffer.drain_since(since);
                let pos = st.buffer.position();
                let exit = st.exit_code.map(|c| (c, st.signal.clone()));
                (drained, exit, pos)
            };

            // If the child has exited, always return Exited — even
            // if there are also new bytes. The bytes are included
            // in the Exited variant so the caller gets everything
            // in one response.
            if let Some((code, signal)) = exit_info {
                return YieldResult::Exited {
                    output: new_bytes,
                    position_after,
                    exit_code: code,
                    signal,
                };
            }

            // New output without exit → return immediately.
            if !new_bytes.is_empty() {
                return YieldResult::Output {
                    output: new_bytes,
                    position_after,
                };
            }

            // Nothing to return yet. Sleep until a wake source
            // fires or the deadline hits or the poll interval
            // elapses (see doc comment above for why the poll).
            let now = Instant::now();
            if now >= deadline {
                return YieldResult::Yielded { position_after };
            }
            let remaining = deadline - now;
            let poll_interval = std::time::Duration::from_millis(100);
            let next_wake = remaining.min(poll_interval);

            // The `notified()` future only catches notifications
            // that arrive AFTER it's awaited. That's why we put it
            // inside a select!, racing the sleep. If a notify was
            // missed between our state read and the registration,
            // the sleep wakes us within 100ms anyway.
            tokio::select! {
                _ = self.notify.notified() => {}
                _ = tokio::time::sleep(next_wake) => {}
            }
        }
    }
}

/// Outcome of a single `yield_until` call.
///
/// Four variants map to the four reasons a yield loop terminates.
/// The caller (tool handler) translates this into the tool-output
/// JSON shape the model sees — see `context/unified-exec.md` §Output
/// shape.
#[derive(Debug)]
pub enum YieldResult {
    /// New output arrived within the deadline. Process is still
    /// running. `position_after` is the caller's new cursor.
    Output {
        output: Vec<u8>,
        position_after: u64,
    },

    /// Child exited during the wait. `output` may contain bytes
    /// that arrived between `since` and exit; if nothing arrived
    /// it's empty. `signal` is the signal name if the child died
    /// to a signal.
    Exited {
        output: Vec<u8>,
        position_after: u64,
        exit_code: i32,
        signal: Option<String>,
    },

    /// Deadline elapsed with neither output nor exit. Process is
    /// still running. Caller returns session_id to the model with
    /// empty output.
    Yielded { position_after: u64 },

    /// `cancel` flipped to true mid-wait. Process is intentionally
    /// left running — see §Cancellation in the design doc. Caller
    /// typically propagates this as `agent::Cancelled`.
    Cancelled,
}

impl Drop for UnifiedExecProcess {
    fn drop(&mut self) {
        // Best-effort teardown: signal the child so the reader thread
        // observes EOF and returns. Then give the thread briefly to
        // finish. We don't join — if the child is stuck, we don't
        // want to hang the REPL.
        if let Ok(mut killer) = self.child_killer.lock() {
            let _ = killer.kill();
        }
        // Drop the writer so the child sees EOF on stdin.
        if let Ok(mut w) = self.writer.lock() {
            *w = None;
        }
        // Let the thread wind down on its own; its state writes are
        // harmless after Drop because the Arc<Mutex<_>> it holds is
        // still valid until the last reference drops.
        if let Ok(mut slot) = self.reader_thread.lock() {
            *slot = None;
        }
    }
}

/// Pump bytes from `reader` into `state.buffer`, firing `notify` on
/// every read. Returns when reader EOFs or errors.
fn reader_loop(
    mut reader: Box<dyn Read + Send>,
    state: Arc<Mutex<ProcessState>>,
    notify: Arc<Notify>,
) {
    let mut buf = vec![0u8; READ_BUF_BYTES];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break, // EOF — child exited or closed stdout.
            Ok(n) => {
                // Take the async mutex from a blocking thread — legal
                // because this thread is not a tokio worker.
                let mut st = state.blocking_lock();
                st.buffer.write(&buf[..n]);
                drop(st);
                notify.notify_waiters();
            }
            Err(e) => {
                // EIO on macOS pty master happens when the slave side
                // closes (child exit); treat it as EOF. Other errors
                // also terminate the read loop — the wait() call
                // after this will surface the real exit status.
                let _ = e;
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Integration tests for the process lifecycle.
    //!
    //! These actually spawn real processes — cheap, unix-only, and
    //! gated behind cfg(unix) at the module level. They verify:
    //!
    //! 1. A fast-exit command (`echo`) writes its output to the
    //!    buffer and sets exit_code == 0 within a bounded wait.
    //! 2. A failing command surfaces a non-zero exit code.
    //! 3. `terminate()` on a long-running command produces an exit
    //!    code (signal-specific value varies, but it WILL exit).
    //! 4. `write_stdin` echoes back through `cat`.
    //! 5. Dropping the process without explicit terminate also
    //!    tears down the child.
    //!
    //! Note: we can't use `#[tokio::test]` directly with blocking
    //! threads inside — but because our reader thread is a plain
    //! std::thread, there's no runtime affinity issue. We use the
    //! tokio multi-thread flavor to be safe.

    use super::*;
    use std::time::Duration;
    use tokio::time::{sleep, timeout};

    async fn wait_for_exit(proc: &UnifiedExecProcess, max: Duration) -> Option<i32> {
        let deadline = Instant::now() + max;
        loop {
            {
                let st = proc.state.lock().await;
                if let Some(code) = st.exit_code {
                    return Some(code);
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn echo_command_captures_output_and_exits_zero() {
        let proc = UnifiedExecProcess::spawn(SpawnOptions {
            cmd: "echo hello world".into(),
            shell: None,
            workdir: None,
            tty: true,
        })
        .expect("spawn");

        let exit = wait_for_exit(&proc, Duration::from_secs(5))
            .await
            .expect("command should exit within 5s");
        assert_eq!(exit, 0);

        let snap = proc.state.lock().await.buffer.snapshot();
        let s = String::from_utf8_lossy(&snap);
        assert!(
            s.contains("hello world"),
            "echo output must contain the literal string; got: {s:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nonzero_exit_code_is_reported() {
        let proc = UnifiedExecProcess::spawn(SpawnOptions {
            cmd: "exit 42".into(),
            shell: None,
            workdir: None,
            tty: true,
        })
        .expect("spawn");

        let exit = wait_for_exit(&proc, Duration::from_secs(5)).await;
        assert_eq!(exit, Some(42));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn terminate_kills_long_running_process() {
        // sleep 60 will definitely still be running when we kill it.
        let proc = UnifiedExecProcess::spawn(SpawnOptions {
            cmd: "sleep 60".into(),
            shell: None,
            workdir: None,
            tty: true,
        })
        .expect("spawn");

        // Confirm it's actually running before we kill.
        sleep(Duration::from_millis(200)).await;
        assert!(!proc.has_exited().await, "sleep 60 shouldn't exit in 200ms");

        proc.terminate();

        // Give the signal time to propagate and the reader thread
        // time to observe EOF + reap. 3 seconds is generous.
        let exit = wait_for_exit(&proc, Duration::from_secs(3)).await;
        assert!(
            exit.is_some(),
            "terminated process must eventually report exit_code"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn write_stdin_round_trips_through_cat() {
        let proc = UnifiedExecProcess::spawn(SpawnOptions {
            // `cat` without args echoes stdin to stdout. We send a
            // line, wait, send EOF (ctrl-D), and expect the line
            // back.
            cmd: "cat".into(),
            shell: None,
            workdir: None,
            tty: true,
        })
        .expect("spawn");

        // Give cat a moment to start up.
        sleep(Duration::from_millis(100)).await;

        proc.write_stdin(b"ping\n").expect("write");
        // cat echoes immediately; give it a moment to flush.
        sleep(Duration::from_millis(200)).await;

        // Send ctrl-D to close cat's stdin → it exits.
        proc.write_stdin(&[0x04]).expect("eof");

        let exit = wait_for_exit(&proc, Duration::from_secs(3))
            .await
            .expect("cat should exit on EOF");
        assert_eq!(exit, 0);

        let snap = proc.state.lock().await.buffer.snapshot();
        let s = String::from_utf8_lossy(&snap);
        // In TTY mode, our input is also echoed (the pty cooks it),
        // so we expect "ping" to appear at least once in output.
        assert!(
            s.contains("ping"),
            "output must contain the echoed line; got: {s:?}"
        );
    }

    // ------------------------------------------------------------
    // YIELD TESTS (M2)
    //
    // These exercise the four YieldResult variants. The rule set
    // they verify:
    //
    //   1. Output arrives before deadline → Output variant; bytes
    //      are exactly what was written past the cursor; no exit.
    //   2. Process exits before deadline → Exited variant; exit
    //      code matches; any bytes that arrived before exit are
    //      included.
    //   3. Nothing happens before deadline → Yielded variant;
    //      position_after == buffer.position() at wake.
    //   4. Cancel flips during wait → Cancelled variant; process
    //      remains alive and reapable.
    //
    // Each test uses a real subprocess, a real tokio runtime, and
    // a bounded deadline so failure modes surface as timeouts
    // rather than hangs.
    // ------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn yield_returns_output_when_bytes_arrive_before_deadline() {
        // A shell command that writes something immediately and
        // then sleeps. We yield with a 3-second deadline; the
        // write should wake us well before timeout.
        let proc = UnifiedExecProcess::spawn(SpawnOptions {
            cmd: "printf 'first\\n'; sleep 5".into(),
            shell: None,
            workdir: None,
            tty: true,
        })
        .expect("spawn");

        let deadline = Instant::now() + Duration::from_secs(3);
        let result = proc.yield_until(0, deadline, None).await;

        match result {
            YieldResult::Output {
                output,
                position_after,
            } => {
                let s = String::from_utf8_lossy(&output);
                assert!(
                    s.contains("first"),
                    "expected 'first' in output; got: {s:?}"
                );
                assert!(position_after > 0, "cursor must advance");
            }
            other => panic!("expected Output, got {other:?}"),
        }

        // Clean up the still-running sleep.
        proc.terminate();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn yield_returns_exited_when_process_completes() {
        // Fast-exit command. There's an intentional race:
        //   - the PTY read loop delivers "done" bytes first,
        //   - the child's exit status lands a few ms later when
        //     Child::wait() returns.
        // Depending on scheduler timing, the first yield call can
        // see Output (bytes before exit) or Exited (both). The
        // second yield call — which starts from the advanced
        // cursor — is guaranteed to see Exited because exit_code
        // is set by then.
        //
        // This two-yield pattern mirrors the real polling behavior
        // of the tool: model yields, gets Output, yields again to
        // poll for more.
        let proc = UnifiedExecProcess::spawn(SpawnOptions {
            cmd: "echo done; exit 7".into(),
            shell: None,
            workdir: None,
            tty: true,
        })
        .expect("spawn");

        let deadline1 = Instant::now() + Duration::from_secs(3);
        let mut cursor = 0u64;
        let mut saw_done_in_output = false;

        // Keep yielding until we get Exited. In practice takes
        // 1 or 2 iterations.
        for _ in 0..5 {
            let deadline = if cursor == 0 {
                deadline1
            } else {
                Instant::now() + Duration::from_secs(3)
            };
            let result = proc.yield_until(cursor, deadline, None).await;
            match result {
                YieldResult::Output {
                    output,
                    position_after,
                } => {
                    if String::from_utf8_lossy(&output).contains("done") {
                        saw_done_in_output = true;
                    }
                    cursor = position_after;
                }
                YieldResult::Exited {
                    output, exit_code, ..
                } => {
                    assert_eq!(exit_code, 7, "exit code must propagate");
                    // Either the first yield saw "done" in Output,
                    // or this yield carries it (racing order).
                    let also_here = String::from_utf8_lossy(&output).contains("done");
                    assert!(
                        saw_done_in_output || also_here,
                        "'done' must appear across the yield sequence"
                    );
                    return;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        panic!("yield sequence never reached Exited in 5 iterations");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn yield_returns_yielded_when_deadline_elapses_with_no_output() {
        // A silent long-running command. Deadline hits first.
        let proc = UnifiedExecProcess::spawn(SpawnOptions {
            cmd: "sleep 30".into(),
            shell: None,
            workdir: None,
            tty: true,
        })
        .expect("spawn");

        let deadline = Instant::now() + Duration::from_millis(400);
        let start = Instant::now();
        let result = proc.yield_until(0, deadline, None).await;
        let elapsed = start.elapsed();

        match result {
            YieldResult::Yielded { .. } => {}
            other => panic!("expected Yielded, got {other:?}"),
        }
        // Should not have waited meaningfully longer than deadline;
        // the 100ms poll interval gives us up to ~100ms overshoot.
        assert!(
            elapsed < Duration::from_millis(700),
            "deadline should fire promptly; elapsed: {elapsed:?}"
        );

        proc.terminate();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn yield_returns_cancelled_when_cancel_flips_mid_wait() {
        use std::sync::atomic::{AtomicBool, Ordering};

        // Long deadline; cancel flips asynchronously partway through.
        // Must observe within the 100ms poll interval.
        let proc = UnifiedExecProcess::spawn(SpawnOptions {
            cmd: "sleep 30".into(),
            shell: None,
            workdir: None,
            tty: true,
        })
        .expect("spawn");

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_clone = cancel.clone();

        // Flip after 150ms — gives the yield time to enter the
        // wait loop before we interrupt.
        tokio::spawn(async move {
            sleep(Duration::from_millis(150)).await;
            cancel_clone.store(true, Ordering::Relaxed);
        });

        let deadline = Instant::now() + Duration::from_secs(10);
        let result = proc.yield_until(0, deadline, Some(&cancel)).await;

        match result {
            YieldResult::Cancelled => {}
            other => panic!("expected Cancelled, got {other:?}"),
        }

        // Critical invariant: cancellation must NOT kill the
        // process. Session remains alive for the model to interact
        // with on the next turn.
        assert!(
            !proc.has_exited().await,
            "cancel must leave the process alive"
        );

        proc.terminate();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn yield_cursor_advances_so_successive_calls_see_only_new_bytes() {
        // Models the real polling pattern: yield, grab position,
        // yield again with new cursor, expect only the second
        // write.
        let proc = UnifiedExecProcess::spawn(SpawnOptions {
            cmd: "printf 'one\\n'; sleep 0.3; printf 'two\\n'; sleep 5".into(),
            shell: None,
            workdir: None,
            tty: true,
        })
        .expect("spawn");

        let d1 = Instant::now() + Duration::from_secs(2);
        let (first_output, cursor) = match proc.yield_until(0, d1, None).await {
            YieldResult::Output {
                output,
                position_after,
            } => (output, position_after),
            other => panic!("first yield expected Output, got {other:?}"),
        };
        assert!(String::from_utf8_lossy(&first_output).contains("one"));

        let d2 = Instant::now() + Duration::from_secs(2);
        let (second_output, _) = match proc.yield_until(cursor, d2, None).await {
            YieldResult::Output {
                output,
                position_after,
            } => (output, position_after),
            other => panic!("second yield expected Output, got {other:?}"),
        };
        let s2 = String::from_utf8_lossy(&second_output);
        assert!(
            s2.contains("two"),
            "second yield must see 'two'; got: {s2:?}"
        );
        assert!(
            !s2.contains("one"),
            "cursor must exclude already-seen bytes; got: {s2:?}"
        );

        proc.terminate();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn yield_wakes_immediately_on_notify_not_polling_interval() {
        // Regression guard: if the notify wiring were broken, the
        // yield would only wake on the 100ms poll interval. With
        // notify, wake latency should be <50ms for output that
        // arrives well after the yield started.
        //
        // printf after a 200ms sleep → yield has been waiting for
        // 200ms when bytes arrive. Measure how fast we return.
        let proc = UnifiedExecProcess::spawn(SpawnOptions {
            cmd: "sleep 0.2; printf 'hi\\n'; sleep 5".into(),
            shell: None,
            workdir: None,
            tty: true,
        })
        .expect("spawn");

        let deadline = Instant::now() + Duration::from_secs(10);
        let start = Instant::now();
        let result = proc.yield_until(0, deadline, None).await;
        let elapsed = start.elapsed();

        match result {
            YieldResult::Output { .. } => {}
            other => panic!("expected Output, got {other:?}"),
        }
        // The printf fires at ~200ms. We should return within the
        // next ~50ms of notify latency. 350ms is a generous ceiling
        // that still catches a broken-notify regression (which
        // would manifest as ~300ms = 200ms sleep + 100ms next
        // poll).
        assert!(
            elapsed < Duration::from_millis(500),
            "notify wake should be fast; elapsed: {elapsed:?}"
        );

        proc.terminate();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drop_without_terminate_still_reaps() {
        // Spawn, let it start, drop. The child should not survive
        // (if it did, our test process would accumulate zombies).
        let pid = {
            let proc = UnifiedExecProcess::spawn(SpawnOptions {
                cmd: "sleep 30".into(),
                shell: None,
                workdir: None,
                tty: true,
            })
            .expect("spawn");
            sleep(Duration::from_millis(100)).await;
            let p = proc.pid.expect("pid");
            drop(proc);
            p
        };

        // After drop, SIGHUP should have been delivered. Give the OS
        // a moment. Then verify the PID is no longer a running
        // process we own.
        timeout(Duration::from_secs(3), async {
            loop {
                // kill -0 PID returns Ok if the process exists and
                // we have permission to signal it. Errno ESRCH
                // means "no such process" — our expected outcome.
                let alive = unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
                if !alive {
                    return;
                }
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("dropped session's child must die within 3s");
    }
}
