# In-process context: env → explicit runtime + `AppContext`

Plan to reduce or remove **in-process** use of `SIDEKAR_*` environment variables in favor of explicit `ProcessRuntime` / `AppContext` fields. Child-process inheritance and OS/deployment env remain valid uses of `std::env`.

## Current inventory

### A. In-process “context” (candidates to replace)

| Variable | Set where | Read where | Role |
|----------|-----------|------------|------|
| `SIDEKAR_VERBOSE` | `main`, implied by PTY | `main`, `repl`, `pty`, `daemon` | Debug logging |
| `SIDEKAR_PTY` | `pty` (child env) | `main` | Running under PTY wrapper |
| `SIDEKAR_AGENT_NAME` | `repl`, `cron`, `pty` | `lib` (`last_session_file`, `is_named_agent`), `commands` (`recovered_bus_state`, cron `created_by`), warnings | Bus identity + session file isolation |
| `SIDEKAR_CHANNEL` | `pty`, `cron` | Broker lookup paths | Tie-in to agent session |
| `SIDEKAR_CRON_DEPTH` | `cron` | `cron` | Re-entrancy guard |
| `CDP_PORT` | Often external | `main`, `core` | Dev override |

### B. Already non-env or hybrid

- **`providers::VERBOSE`** is an `AtomicBool` — good pattern.
- `repl` still checks `SIDEKAR_VERBOSE`; `main` still sets env. That is redundant with the atomic.

### C. Should stay env (or external contract)

- **`CHROME_PATH`, `HOME`, API keys**, `SIDEKAR_API_URL` / `SIDEKAR_RELAY_URL`: OS integration, deployment, secrets.
- **PTY-spawned child**: today the child **inherits** `SIDEKAR_PTY`, `SIDEKAR_AGENT_NAME`, etc. Replacing that requires a new contract (wrapper file, `--internal-json`, fd, IPC) — a separate, larger change.

---

## Proposed architecture: two layers

### 1. `ProcessRuntime` (or `SidekarFlags`) — `OnceLock` + atomics

Single module, e.g. `src/runtime.rs`:

- Initialized **once at process start** from:
  - CLI (`--verbose`), and
  - optionally **env fallback** for PTY children only (`SIDEKAR_*`) until the wrapper passes flags another way.
- Example API:
  - `runtime::verbose() -> bool`
  - `runtime::agent_name() -> Option<&str>` (or owned clone)
  - `runtime::pty_mode() -> bool`
  - `runtime::cron_depth()` / `runtime::enter_cron_action()` for the depth guard

**Why `OnceLock`:** `daemon`, `pty` (parent), and `main` do not all carry `AppContext`, but they share one process and one logical runtime.

**Verbose:** `main` calls `runtime::init_from_cli(...)` and `providers::set_verbose(runtime::verbose())` instead of `unsafe { set_var("SIDEKAR_VERBOSE", ...) }`.

### 2. Extend `AppContext` for command-specific state

Add fields that **default from runtime** but can be **overridden per dispatch** (cron already builds `AppContext` via `CronContext::to_app_context()`):

- `verbose: bool`
- `agent_name: Option<String>`
- `pty_mode: bool`
- Optional: `agent_channel: Option<String>`

Then:

- `AppContext::last_session_file()` / `is_named_agent()` use **`self.agent_name`**, not `env::var`.
- `recovered_bus_state(ctx: &AppContext)` (or `ctx.bus_state()`) uses **`ctx.agent_name`** instead of env.
- **Cron:** stop `set_var` in `execute_cron_job`; ensure `to_app_context()` sets `agent_name` / channel from `CronContext` (data already present).
- **Repl:** stop `set_var` for the bus name; call `runtime::set_agent_name(...)` once, or set `agent_name` on every `AppContext` used for `dispatch`.

Sites that only read env today (e.g. bus warnings in `commands/mod.rs`) switch to `runtime::agent_name().is_none()`.

---

## Migration order (low risk → higher)

1. **Verbose:** `main` → `runtime::init` + `providers::set_verbose`; remove `SIDEKAR_VERBOSE` **sets**; replace **reads** with `providers::verbose()` or `runtime::verbose()`. (PTY child can keep env read only inside `runtime::init` as fallback until the wrapper is updated.)
2. **`SIDEKAR_CRON_DEPTH`:** replace with `runtime::cron_depth` atomic or a small guard type; no env.
3. **`SIDEKAR_AGENT_NAME` / channel:** thread through `AppContext` + runtime init from repl / cron / PTY parent; **`lib` session paths** stop using env when `ctx` is available.
4. **PTY child:** last step — either keep env for the child only, or add an explicit handshake so the child calls `runtime::init_from_pty_child()` from argv/fd.

---

## Strict vs pragmatic rollout

- **Strict:** After migration, no `SIDEKAR_*` reads in-process except inside a single `runtime::init` that is only used where needed.
- **Pragmatic:** `runtime::init` merges CLI + env for PTY-spawned children until the PTY path passes flags explicitly.

---

## Rationale

In-process flags belong in **explicit `AppContext` + a small process-wide runtime** (`OnceLock` / atomics) instead of mutating global `environ`, which is `unsafe` in modern Rust and hard to reason about under concurrency. Keeping env **only** for subprocess inheritance and OS/secret configuration matches common Rust CLI/service practice.

---

## Current status after first pass

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

## Remaining work

### 1. PTY child bootstrap contract

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

### 2. Channel as explicit app context

`agent_name` is now explicit on `AppContext`, but `channel` is still mostly runtime/child-contract state.

Consider adding:

- `agent_channel: Option<String>`

to `AppContext` if command paths need it explicitly the way they now need `agent_name`.

### 3. Parallel test isolation

`cargo test -- --test-threads=1` passes cleanly, but default parallel `cargo test` still exposes proxy temp-file/test-isolation races.

Follow-up:

- make proxy tests safe under parallel execution
- ensure temp CA files / HOME overrides are isolated correctly

### 4. Small cleanup

- Remove the unnecessary `mut` warning in `src/events.rs`.

## Important design note

`SIDEKAR_CRON_DEPTH` was originally called out as “simple,” but the real issue is broader:

- in-process state should not be ambient global/env state
- child inheritance should be explicit to that child, not a mutation of the parent process environment

That is the design rule this refactor should continue following.
