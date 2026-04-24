# Unified Exec: Long-Running Shell Sessions for the Agent

Status: draft spec, pre-implementation. Branch: `unified-exec`.

Owner: sidekar REPL agent.

---

## Problem

Today the `Bash` tool is strictly synchronous: spawn, wait with a timeout
(120s default, 600s max), return combined stdout+stderr. This forces every
shell interaction to complete within a single tool call. Anything longer is
either impossible or requires ugly workarounds:

1. **Dev servers** (`npm run dev`, `cargo watch`, `flask run`, `vite`) —
   the agent cannot start one and leave it running for subsequent turns.
2. **Interactive REPLs** (`python -i`, `node`, `psql`, `irb`) — the agent
   cannot spawn one, send a statement, read the result, send another.
3. **Long-running builds/tests** beyond 10 minutes — killed by timeout
   even if they would succeed.
4. **Log tailing** (`tail -f`, `kubectl logs -f`) — cannot poll then
   terminate.
5. **SSH sessions, tmux attach, vim, less** — need a real TTY; `Bash`
   pipes stdio.

The agent cannot escape with `cmd &` because the subprocess holds
stdout/stderr pipes open until it exits, and `wait_with_output` blocks on
pipe close. Users can coax the model into `cmd >/dev/null 2>&1 &` which
does detach, but then output is lost and there is no way to poll, kill, or
check on the process. That is not a first-class feature; it is a hack that
also bypasses the agent's existing process-group cleanup.

## Non-goals

- Replacing the existing `Bash` tool. It stays. It is the right abstraction
  for short, self-contained commands (the 90% case).
- Sandboxing / approval gates. Codex's `unified_exec` has them; sidekar
  does not have that layer today and adding it is out of scope.
- Cross-session process persistence. If the REPL exits, all sessions
  die. No "reattach to the process from yesterday."
- Windows. Initial implementation is unix-only (`portable-pty` supports
  Windows, but PTY semantics on Windows diverge enough that it warrants
  a follow-up).

## Design inspiration

Codex's `unified_exec` tool pair (`~/src/oss/codex/codex-rs/core/src/unified_exec/`
plus `core/src/tools/handlers/unified_exec.rs`, `tools/src/local_tool.rs`).
~3.2k LOC of production code we are studying, not copying verbatim.

**Not** Claude Code's model. Claude Code exposes `run_in_background: true`
on the regular Bash tool plus `BashOutput` and `KillBash` companions. That
is simpler (~200 LOC) but uses pipes not PTY, so interactive REPLs,
curses UIs, and anything TTY-needing do not work. For a coding agent that
wants to run `python -i` or `vim` or a dev server, PTY is the right
substrate.

## Model-facing interface

Two new tools. Both live alongside `Bash` — the model picks based on
whether it needs a persistent session.

### Tool 1: `ExecSession`

Spawn a command in a PTY and return after `yield_time_ms` whether or not
the command finished.

**Input schema:**
```json
{
  "type": "object",
  "properties": {
    "cmd": {
      "type": "string",
      "description": "Shell command to execute."
    },
    "workdir": {
      "type": "string",
      "description": "Working directory. Defaults to the REPL's cwd."
    },
    "shell": {
      "type": "string",
      "description": "Shell binary. Defaults to $SHELL, else /bin/bash."
    },
    "tty": {
      "type": "boolean",
      "description": "Allocate a PTY. Default true. Set false for pipe-only output."
    },
    "yield_time_ms": {
      "type": "integer",
      "description": "How long to wait for output before yielding control back. Default 10000. Range 250-30000."
    },
    "max_output_tokens": {
      "type": "integer",
      "description": "Cap on returned output in tokens. Default 10000. Excess is head-tail truncated."
    }
  },
  "required": ["cmd"]
}
```

**Output shape** (returned as a JSON-encoded string in the tool result):
```json
{
  "output": "<bytes captured during this call>",
  "wall_time_seconds": 10.0,
  "session_id": 42,
  "exit_code": null,
  "original_token_count": 3421
}
```

- `session_id` is present iff the process is still alive. The model must
  include it in subsequent `WriteStdin` calls.
- `exit_code` is present iff the process finished during this call (the
  mutually-exclusive case).
- If the command completes before `yield_time_ms`, it returns immediately
  with no `session_id`.

### Tool 2: `WriteStdin`

Send input to a live session and/or poll for more output.

**Input schema:**
```json
{
  "type": "object",
  "properties": {
    "session_id": {
      "type": "integer",
      "description": "Session id returned by ExecSession."
    },
    "chars": {
      "type": "string",
      "description": "Bytes to send to stdin. Empty string means just poll."
    },
    "yield_time_ms": {
      "type": "integer",
      "description": "How long to wait for output before yielding. Default 250 for non-empty input, 5000 for empty (poll). Range 250-30000."
    },
    "max_output_tokens": {
      "type": "integer",
      "description": "Cap on returned output. Default 10000."
    }
  },
  "required": ["session_id"]
}
```

**Output shape:** same as `ExecSession`, minus the need to return
`session_id` when the process just exited (caller already has it).

### Tool 3: `KillSession`

Kill a session cleanly (SIGTERM then SIGKILL after 500ms).

**Input schema:**
```json
{
  "type": "object",
  "properties": {
    "session_id": { "type": "integer" }
  },
  "required": ["session_id"]
}
```

**Output:** `{ "killed": true, "exit_code": -15 }` or an error if the
session id is unknown (already reaped).

### Tool 4: `ListSessions`

Enumerate live sessions so the model can recover after a forgotten id.
Cheap, always safe.

**Input schema:** `{}`

**Output:**
```json
{
  "sessions": [
    {
      "session_id": 42,
      "command": "npm run dev",
      "started_at_unix": 1745500000,
      "age_seconds": 127.5,
      "buffer_bytes": 18234,
      "alive": true
    }
  ]
}
```

## Why four tools, not one

Codex bundles kill into session teardown via the PTY dropping naturally
and ids expiring. Claude Code keeps them separate. Separate is clearer
for the model — it can see exactly what actions exist. Cost is trivial
(four schemas instead of one). `ListSessions` is an insurance policy
against the model losing a session id mid-turn; without it, a live
process would be unreachable.

## Internal architecture

New module: `src/agent/unified_exec/`.

```
src/agent/unified_exec/
  mod.rs              # Public surface: ProcessManager, new(), spawn(), write_stdin(), kill(), list()
  process.rs          # UnifiedExecProcess: wraps portable-pty child + reader tasks
  buffer.rs           # HeadTailBuffer: bounded output capture
  manager.rs          # ProcessStore + ProcessManager: id allocation, store, cleanup
  errors.rs           # UnifiedExecError
  tests.rs            # Unit tests (buffer, manager, process)
```

### `portable-pty` dependency

Add `portable-pty = "0.9"` to `Cargo.toml`. This is the de-facto Rust PTY
crate (WezTerm's own, used by Codex, Warp, Zellij). Pure Rust on unix,
wraps ConPTY on Windows. ~800 LOC including deps, no C bindings.

### HeadTailBuffer

Bounded ring-like buffer that preserves the first N bytes and last M
bytes of output and drops the middle. Same strategy as Codex's
`head_tail_buffer.rs`.

```rust
pub struct HeadTailBuffer {
    head: Vec<u8>,           // first HEAD_CAP bytes
    tail: VecDeque<u8>,      // last TAIL_CAP bytes
    total_bytes_written: u64,
    dropped: bool,
}
```

Constants (mirror Codex):
- `HEAD_CAP: usize = 64 * 1024;`  (64 KiB)
- `TAIL_CAP: usize = 960 * 1024;`  (960 KiB) — so total cap is ~1 MiB
- `UNIFIED_EXEC_OUTPUT_MAX_TOKENS: usize = 10_000;`

When writing exceeds the caps, the middle is lost and a one-time flag
set. On `drain_since(position)`:

1. Read from the tail at the given logical position to current end.
2. If the caller's position is below the head cap, also include head.
3. If `dropped`, insert a marker `\n[... N bytes truncated ...]\n`.

This lets the model see the command's startup banner (head) AND the most
recent output (tail) even for a 5-minute-running dev server.

### UnifiedExecProcess

```rust
pub struct UnifiedExecProcess {
    pid: u32,
    session_id: i32,
    command: Vec<String>,
    started_at: Instant,

    // PTY
    pty_master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,

    // Shared output state — the reader task writes, tool handlers drain.
    state: Arc<Mutex<ProcessState>>,

    // Signals the reader task to stop; set by Drop/kill.
    shutdown: Arc<AtomicBool>,
    reader_handle: Option<JoinHandle<()>>,
}

pub struct ProcessState {
    buffer: HeadTailBuffer,
    exit_code: Option<i32>,
    // Broadcast used by yield_until to wake on new output or exit.
    notify: Arc<Notify>,
}
```

**Spawn sequence** (in `process::spawn`):

1. Build `CommandBuilder` from `cmd`, `shell`, `workdir`.
2. `pty_system.openpty(PtySize { rows: 24, cols: 200, ... })`.
3. `slave.spawn_command(cmd_builder)` → child.
4. `master.try_clone_reader()` → reader stream.
5. Spawn a tokio task:
   ```
   loop {
       match reader.read(&mut buf).await {
           Ok(0) => break,               // EOF = child exited
           Ok(n) => {
               let mut state = state.lock().await;
               state.buffer.write(&buf[..n]);
               state.notify.notify_waiters();
           }
           Err(_) => break,
       }
       if shutdown.load(Relaxed) { break; }
   }
   // Reap exit code.
   let status = child.wait().await;
   let mut state = state.lock().await;
   state.exit_code = Some(status.exit_code() as i32);
   state.notify.notify_waiters();
   ```
6. Drop slave pty in parent (child keeps it).
7. Return `Arc<UnifiedExecProcess>`.

**Yield semantics** (`yield_until`):

```rust
async fn yield_until(&self, deadline: Instant, since: u64) -> YieldResult {
    loop {
        let (new_output, exit_code) = {
            let state = self.state.lock().await;
            let out = state.buffer.drain_since(since);
            (out, state.exit_code)
        };
        if exit_code.is_some() { return YieldResult::Exited { ... }; }
        if !new_output.is_empty() { return YieldResult::Output { new_output }; }
        if Instant::now() >= deadline { return YieldResult::Yielded; }
        tokio::select! {
            _ = self.state.lock().await.notify.notified() => {}
            _ = tokio::time::sleep_until(deadline) => {}
        }
    }
}
```

The `notify` trick means the yield returns **immediately** on the first
byte of output OR the child's exit, OR when the deadline hits. No busy
polling.

### ProcessManager

```rust
pub struct ProcessManager {
    store: Mutex<ProcessStore>,
    next_id: AtomicI32,
}

struct ProcessStore {
    processes: HashMap<i32, Arc<UnifiedExecProcess>>,
}
```

- `spawn(...) -> Result<(session_id, initial_yield_result)>`
- `write_stdin(session_id, bytes, yield_time) -> Result<YieldResult>`
- `kill(session_id) -> Result<i32>`
- `list() -> Vec<SessionInfo>`
- `reap_exited()` — called opportunistically before each spawn: iterates
  store, removes entries whose `exit_code` is set. Keeps `MAX_SESSIONS`
  from filling with zombies.

**Capacity cap**: `MAX_SESSIONS = 32`. (Codex uses 64; sidekar's REPL is
a single user, 32 is plenty.) When full, `spawn` returns an error
telling the model to kill or wait for one to finish.

**ID allocation**: monotonic `AtomicI32` starting at 1. No recycling —
we never hand out the same id twice per process lifetime. Wrapping at
i32::MAX is hypothetically possible; in practice the REPL will restart
first.

### Lifecycle ownership

`ProcessManager` lives in `AppContext` alongside existing state:

```rust
pub struct AppContext {
    // ... existing fields
    pub exec_sessions: Arc<ProcessManager>,
}
```

Created once on REPL startup. On REPL exit, `Drop` on `ProcessManager`
iterates the store and kills every live process (SIGTERM with 500ms
grace, then SIGKILL). Reader tasks see `shutdown=true` and exit.

## Integration with existing agent plumbing

### `src/agent/tools.rs`

Add three entries to `definitions()` after `Bash`:
1. `ExecSession` — description explains: "Use this for long-running
   commands (dev servers, REPLs, interactive tools) you want to come
   back to. Returns a session_id if the command is still running after
   yield_time_ms. Use WriteStdin to send input or poll, KillSession to
   terminate."
2. `WriteStdin`
3. `KillSession`
4. `ListSessions`

Add matches to `execute()`:
```rust
"ExecSession" | "exec_session" => exec_exec_session(arguments, ctx).await,
"WriteStdin" | "write_stdin" => exec_write_stdin(arguments, ctx).await,
"KillSession" | "kill_session" => exec_kill_session(arguments, ctx).await,
"ListSessions" | "list_sessions" => exec_list_sessions(ctx).await,
```

**Plumbing note:** the current `execute()` takes `(name, arguments,
cancel)`. The new tools need access to the `ProcessManager`. Option A:
add a `ctx: &AppContext` parameter to `execute` (ripples through the
call site in `src/agent/mod.rs`). Option B: put `ProcessManager` in a
process-global `OnceLock`.

**Pick A.** Cleaner, testable, no globals. The ripple is one call site.

### Cancellation (`Cancelled` / Esc)

When the user presses Esc mid-turn, the existing `cancel` AtomicBool
fires. For `ExecSession` / `WriteStdin` that are currently yielding:

- The yield loop checks `cancel` on every wake. If set, returns
  `Cancelled` **without killing the process**. The session remains
  alive with its id intact. The model sees the tool call cancelled but
  the process is still listed in `ListSessions`.

Why not kill on Esc? Esc cancels the **turn**, not the process. If the
user asked the agent to start `npm run dev` and then pressed Esc
because they wanted to ask a different question, killing the dev server
would be surprising. The model can kill it explicitly with
`KillSession`.

### Output capture and rtk compaction

The existing `Bash` tool pipes output through `rtk::compact_output` to
reduce tokens. For `ExecSession`, rtk compaction does **not** apply:

1. Sessions are often not single-command output — they are interactive
   streams where rtk's heuristics (detect `cargo`/`git`/`npm`) could
   mangle context.
2. Token limiting is already done via `max_output_tokens` + head-tail
   truncation.

Leaving rtk off is the right call for v1. Can revisit if users complain.

## Security / safety considerations

1. **Process escape.** PTY sessions run with the REPL's full privileges
   (no sandbox). Same as `Bash` today. No regression.
2. **Resource exhaustion.** `MAX_SESSIONS = 32` + 1 MiB buffer cap per
   session = 32 MiB worst case. Fine.
3. **Zombie accumulation.** `reap_exited()` runs before each spawn.
   Additionally, the drop handler on `ProcessManager` (REPL exit) kills
   everything.
4. **PTY hijack.** The master fd is held only by us. We never expose it
   to the model or to network.
5. **Signal propagation.** Unlike `run_subprocess_cancellable`, we do
   NOT use `setpgid(0, 0)` — the child inherits our pgid so
   ctrl-c-from-the-terminal handling stays normal. We kill via
   `Child::kill()` (SIGTERM) then `Child::kill()` again after a delay
   (not ideal — portable-pty doesn't expose graceful SIGTERM vs SIGKILL
   distinction cleanly; we may need to reach into the raw pid with
   `nix::sys::signal::kill` for SIGTERM-then-SIGKILL).

## Open questions

1. **Default `tty: true`?** Codex defaults to false. I argue true here:
   the main use case is interactive REPLs and dev servers that behave
   differently under a TTY (color, line-buffering). Models can opt out.
2. **PTY size.** Codex uses 24x200. I'll start there. Could make it
   configurable if a model complains about wrapped output.
3. **Newline normalization.** PTY output arrives with `\r\n`. Some
   sources say normalize to `\n` before returning to the model.
   Codex does NOT normalize. Start without normalization; add if
   output looks weird.
4. **Persist sessions across `/new`.** Probably no — `/new` is a
   conversation reset; keeping rogue `npm run dev`s running feels
   wrong. `/new` should kill all sessions.

## Testing plan

Unit tests (in `src/agent/unified_exec/tests.rs`):

1. `HeadTailBuffer`: write under head cap → no drop; exceed head cap +
   tail fits → head preserved, tail tracks; overflow both → dropped flag
   set; `drain_since(0)` returns head + marker + tail.
2. `ProcessManager::spawn` fast-exit command (`echo hi`) → returns
   exit_code in first yield, no session_id.
3. `ProcessManager::spawn` long-running (`sleep 10`) with
   `yield_time_ms: 200` → returns session_id, no exit_code.
4. `write_stdin` to `cat` → echoes back.
5. `write_stdin` with empty chars + yield → polls, returns new output.
6. `kill` a running `sleep 60` → exit_code present, session removed
   from `list`.
7. `list` with 0 / 1 / 3 sessions → correct count and metadata.
8. `MAX_SESSIONS` cap — spawn 33rd → error.
9. Drop `ProcessManager` with 2 live sessions → both processes
   receive signal and exit within 1s (use `pgrep` to verify).
10. Cancellation during yield → session stays alive after the tool
    call returns Cancelled.

Integration tests (manual, in a test REPL session):
- `ExecSession { cmd: "python -i" }` → session_id.
- `WriteStdin { session_id, chars: "print(2+2)\n" }` → sees "4".
- `ExecSession { cmd: "npm run dev", workdir: "/some/node/proj" }` →
  session_id, agent polls with empty chars + 5s yield to see requests.
- `ListSessions` during active development → all live processes.
- `/new` → all sessions killed.

## Rollout

1. Branch `unified-exec` (this doc, then impl).
2. Feature-gate behind env `SIDEKAR_UNIFIED_EXEC=1` initially so the
   tool definitions are absent by default. Allows incremental dogfood
   without risk.
3. Land on main as OFF-by-default. One week of dogfooding.
4. Flip to ON-by-default. Bump to v3.1.0 (new tool surface is
   minor-bump worthy).
5. Remove the env flag after 2 versions.

## Estimated scope

- `src/agent/unified_exec/` module: ~1400 LOC implementation + ~600
  LOC tests.
- `src/agent/tools.rs` changes: +250 LOC (schemas, dispatchers,
  JSON-shape marshaling).
- `AppContext` plumbing + `/new` integration: ~50 LOC.
- `Cargo.toml`: +1 dep (`portable-pty`).
- Total: ~2000 LOC diff.

Two focused days of work. Realistically three with polish + doc.

## Milestone breakdown

1. **M1** — Buffer + Process + single-shot spawn. No yield, no stdin.
   Validates PTY plumbing. (½ day)
2. **M2** — Yield semantics + Notify wiring. (½ day)
3. **M3** — ProcessManager + store + id allocation + kill + list. (½ day)
4. **M4** — Tool schemas + dispatcher + AppContext plumbing. (½ day)
5. **M5** — Tests + dogfood + docs. (1 day)

Each milestone is a commit. No big-bang.
