# TODO

General product / operational backlog is tracked in Sidekar tasks rather than in scattered markdown files.

Use:

```bash
sidekar tasks list
sidekar tasks list --status=all
sidekar tasks list --scope=all --status=all
```

Imported from the old markdown TODO on 2026-03-31.

---

## Rust: in-process context → explicit runtime + `AppContext`

Plan to reduce or remove **in-process** use of `SIDEKAR_*` environment variables in favor of explicit `ProcessRuntime` / `AppContext` fields. Child-process inheritance and OS/deployment env remain valid uses of `std::env`.

### Current inventory

#### A. In-process “context” (candidates to replace)

| Variable | Set where | Read where | Role |
|----------|-----------|------------|------|
| `SIDEKAR_VERBOSE` | `main`, implied by PTY | `main`, `repl`, `pty`, `daemon` | Debug logging |
| `SIDEKAR_PTY` | `pty` (child env) | `main` | Running under PTY wrapper |
| `SIDEKAR_AGENT_NAME` | `repl`, `cron`, `pty` | `lib` (`last_session_file`, `is_named_agent`), `commands` (`recovered_bus_state`, cron `created_by`), warnings | Bus identity + session file isolation |
| `SIDEKAR_CHANNEL` | `pty`, `cron` | Broker lookup paths | Tie-in to agent session |
| `SIDEKAR_CRON_DEPTH` | `cron` | `cron` | Re-entrancy guard |
| `CDP_PORT` | Often external | `main`, `core` | Dev override |

#### B. Already non-env or hybrid

- `providers::VERBOSE` is an `AtomicBool`.
- `repl` still checked `SIDEKAR_VERBOSE`; `main` still sets env. That is redundant with the atomic and runtime state.

#### C. Should stay env (or external contract)

- `CHROME_PATH`, `HOME`, API keys, `SIDEKAR_API_URL`, `SIDEKAR_RELAY_URL`
- PTY-spawned child bootstrap until there is an explicit child contract

### Target architecture

#### 1. Process runtime (`src/runtime.rs`)

Single process-local runtime initialized once at startup, with:

- `runtime::verbose()`
- `runtime::agent_name()`
- `runtime::pty_mode()`
- `runtime::channel()`
- `runtime::cron_depth()`
- `runtime::enter_cron_action()`

#### 2. Explicit command context (`AppContext`)

Command-scoped state should live on `AppContext`, defaulting from runtime where appropriate.

Current useful explicit field:

- `agent_name: Option<String>`

Likely next explicit field:

- `agent_channel: Option<String>`

### Current status after first pass

Implemented:

- Added `src/runtime.rs` with process-local runtime state for:
  - verbose
  - PTY mode
  - agent name
  - channel
  - cron depth
- `AppContext` now carries `agent_name`.
- `AppContext::last_session_file()` and `is_named_agent()` use explicit context state.
- `commands/mod.rs` now uses `ctx.agent_name` for bus warnings, `created_by`, and recovered bus state.
- `cron` tool/batch dispatch now uses an in-process runtime depth guard instead of parent-global env mutation.
- `cron` bash child inheritance now passes `SIDEKAR_CRON_DEPTH`, `SIDEKAR_AGENT_NAME`, and `SIDEKAR_CHANNEL` only on the spawned child `Command`.
- `main`, `repl`, `daemon`, and `pty` now use runtime state for in-process verbose / PTY checks.

This means the code is now closer to the intended model:

- in-process state uses `AppContext` / runtime
- child inheritance is explicit on the child process
- parent-global env mutation is reduced

### Remaining work

#### 1. PTY child bootstrap contract

Still deferred by design.

Today PTY child startup still uses env as the child contract:

- `SIDEKAR_PTY`
- `SIDEKAR_AGENT_NAME`
- `SIDEKAR_CHANNEL`

The next real step is to replace that with an explicit child bootstrap mechanism:

- argv payload
- temp file / JSON file
- inherited file descriptor
- small bootstrap IPC

This should be designed explicitly rather than patched ad hoc.

#### 2. Channel as explicit app context

`agent_name` is now explicit on `AppContext`, but `channel` is still mostly runtime/child-contract state.

Consider adding:

- `agent_channel: Option<String>`

to `AppContext` if command paths need it explicitly the way they now need `agent_name`.

#### 3. Parallel test isolation

`cargo test -- --test-threads=1` passes cleanly, but default parallel `cargo test` still exposes proxy temp-file/test-isolation races.

Follow-up:

- make proxy tests safe under parallel execution
- ensure temp CA files / HOME overrides are isolated correctly

#### 4. Small cleanup

- Remove the unnecessary `mut` warning in `src/events.rs`.

### Design note

The important rule is:

- in-process state should not be ambient global/env state
- child inheritance should be explicit to that child, not a mutation of the parent process environment

That is the rule future refactors should continue following.
