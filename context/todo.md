# TODO

## High Priority

- [ ] Token usage tracking: side-by-side comparison of Claude, Codex, Sidekar consumption
- [x] **Session journaling** — shipped on branch `journaling` (10 commits). Background idle-triggered LLM summarization, 12-section structured JSON in `session_journals`, resume injection with reference-only framing, memory promoter at threshold 3. Design doc: `context/journaling.md`. 119 new tests (407 total).
- [x] Evaluate adding sidekar as a native tool (avoid skills/SKILL.md ceremony) — REPL ships a native `Sidekar` tool with embedded catalog + operating rules (`src/agent/tools.rs:242`)
- [x] Evaluate adding edit-file and other precision tools (read, grep, glob) — REPL has Read/Write/Edit/Glob/Grep native (`src/agent/tools.rs`)
- [x] Evaluate mempalace integration (https://github.com/milla-jovovich/mempalace) — no major benefits; sidekar already covers dedup, FTS, confidence, supersession
- [x] Persona system for agents — REPL reads `AGENTS.md` from cwd, appends to system prompt (`src/repl/system_prompt.rs`)
- [x] Skills system (agent-defined capabilities) — `/skill <name>` loads SKILL.md from standard agent dirs (claude/codex/gemini/pi/opencode), session-scoped (`src/repl/skills.rs`, `src/repl/slash.rs`)
- [ ] Multi-agent orchestration
- [ ] Test inter-agent communication across machines
- [ ] Marketing strategy: public launch vs private/invite-only
- [x] ~~Terminal adapters~~ — N/A after PTY + REPL approach
- [x] ~~Editor adapters~~ — N/A after PTY + REPL approach
- [x] ~~Desktop app adapters~~ — N/A after PTY + REPL approach
- [x] Clarify and harden first-install signature verification path (`install.sh` bootstrap trust / how signatures are checked before Sidekar is already installed)
- [ ] Publish Chrome extension to Web Store
- [ ] Update website copy

## Medium Priority

- [x] Pluggable output pipeline: `--format=text|json|toon` (default text), with `--json` / `--toon` shorthand. Applies globally via `src/output.rs` / `runtime::output_format`.
- [x] Google login (in addition to GitHub)
- [ ] Session inspection tools (`sidekar sessions`, `sidekar attach`)
- [x] Refactor/security review
- [x] Add nairo/memory integration
- [x] Define `nairo` scope model: project-level vs user-level

## Low Priority / Future

- [x] Review [axi](https://github.com/kunchenguid/axi) for ideas/inspiration — adopted: content-first defaults, aggregate counts, definitive empty states
- [ ] Linux support
- [ ] Windows support

## Explore Later

- [ ] **Port CUA Driver's background desktop automation into sidekar** (`~/src/oss/cua/libs/cua-driver`, MIT). CUA Driver (Swift, macOS 14+) drives native apps **without stealing focus/cursor** — the critical capability sidekar's `src/desktop/` module lacks. Core techniques to port to Rust FFI:
  - **SkyLight private SPI** (`SkyLightEventPost.swift`): `SLEventPostToPid` posts keyboard/mouse events to a specific PID's mach port without cursor warp or focus steal. `SLSEventAuthenticationMessage` envelope makes Chromium accept synthetic keyboard events on macOS 14+. Mouse path skips auth message (needs IOHIDPostEvent route). All resolved via `dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight")` + `dlsym`. Key symbols: `SLEventPostToPid`, `SLEventSetAuthenticationMessage`, `SLEventSetIntegerValueField`, `CGSMainConnectionID`, `CGEventSetWindowLocation`, `_SLPSGetFrontProcess`, `GetProcessForPID`, `SLPSPostEventRecordTo`.
  - **FocusWithoutRaise** (`FocusWithoutRaise.swift`): Ported from yabai. Posts 248-byte synthetic event records via `SLPSPostEventRecordTo` — defocus previous front (`bytes[0x8a]=0x02`), focus target (`bytes[0x8a]=0x01` + window ID at `bytes[0x3c..0x3f]`). Target becomes AppKit-active without WindowServer restack or Space follow. Deliberately skips `SLPSSetFrontProcessWithOptions`.
  - **FocusGuard 3-layer stack** (`FocusGuard.swift`): Layer 1 = AXEnablementAssertion (set `AXManualAccessibility`/`AXEnhancedUserInterface` for Chromium AX tree activation, cached negative for native apps). Layer 2 = SyntheticAppFocusEnforcer (write `AXFocused`/`AXMain` on target window+element before action, restore after). Layer 3 = SystemFocusStealPreventer (reactive — subscribes to `NSWorkspace.didActivateApplicationNotification`, immediately re-activates prior frontmost if target self-activates, zero-delay demote).
  - **MouseInput** (`MouseInput.swift`): NSEvent-bridged CGEvents (Chromium trusts these over raw CGEvents). Two delivery paths fired in sequence: `SLEventPostToPid` (SkyLight, reaches backgrounded targets) + `CGEvent.postToPid` (public, lands on AppKit targets where SkyLight drops). Deliberately skips `.cghidEventTap` (would warp cursor). Auth-signed click recipe: FocusWithoutRaise → mouseMoved → off-screen primer click at `(-1,-1)` (opens Chromium user-activation gate) → real click. Frontmost targets use HID tap path (only route that reaches OpenGL/GHOST viewports like Blender).
  - **KeyboardInput** (`KeyboardInput.swift`): CGEvent keyboard synthesis with `SLEventPostToPid` preferred (routes through `CGSTickleActivityMonitor` which Chromium needs). Unicode typing via `CGEventKeyboardSetUnicodeString` per-character.
  - **WindowCapture** (`WindowCapture.swift`): ScreenCaptureKit-based per-window screenshots with multi-display scale factor detection, `maxImageDimension` resize, window selection by z-index + current Space.
  - **DaemonServer** (`DaemonServer.swift`): Unix domain socket daemon preserving `AppStateEngine` / element index cache between CLI invocations. Element indices are keyed on `(pid, window_id)` — without a daemon, each CLI call starts fresh and indices don't resolve.
  - **27 tools**: list_apps, list_windows, launch_app, screenshot, get_accessibility_tree, get_window_state, click, double_click, right_click, scroll, type_text, type_text_chars, press_key, hotkey, set_value, move_cursor, get_cursor_position, get_screen_size, check_permissions, set/get_config, agent cursor overlay, recording/replay, zoom.
  - **Recording system**: Trajectory capture with click marker rendering, zoom regions, video recording, replay. Agent cursor overlay with Bezier motion paths.
  - Integration options: (a) Port SkyLight/FocusWithoutRaise/FocusGuard to Rust via `dlopen`/`dlsym` (all C-level symbols, no ObjC dependency except NSEvent mouse bridge which needs `objc2`). (b) Ship `cua-driver` binary alongside sidekar and shell out. (c) Hybrid — port the input/focus layer to Rust, keep sidekar's existing AX tree walker.
  - Ref: `~/src/oss/cua/libs/cua-driver/Skills/cua-driver/SKILL.md` has detailed no-foreground contract and self-check patterns.
- [ ] Evaluate [agent-desktop](https://github.com/lahfir/agent-desktop) (`~/src/oss/agent-desktop`) for ref system and skeleton traversal patterns — 53-command CLI with `@e1` refs, staleness detection, progressive traversal (78–96% token reduction), cross-platform `PlatformAdapter` trait. Apache-2.0. Consider cherry-picking ref/skeleton patterns into sidekar's desktop module alongside the CUA Driver background-operation port.
- [ ] [vercel-labs/wterm](https://github.com/vercel-labs/wterm) — DOM-rendered terminal emulator (Zig→WASM), potential xterm.js replacement for web terminal
- [ ] [NousResearch/hermes-agent-self-evolution](https://github.com/NousResearch/hermes-agent-self-evolution) — DSPy+GEPA evolutionary optimization of skills/prompts; concept only (no license, Python); revisit if/when REPL prompt regression evals exist
- [ ] SKILL.md `requires_secrets` frontmatter — parse YAML frontmatter on `/skill <name>` load, check kv via new `kv_exists` (EXISTS query, no decrypt), fail-closed on missing required keys with actionable `sidekar kv set` hints; strip frontmatter from body before injecting into system prompt; values never enter agent context (skill body documents `sidekar kv exec --keys=...` shape). Files: `src/repl/skills.rs`, `src/repl/slash.rs:463-494`, `src/broker/kv_store.rs`. ~200 LOC + tests. Inspired by [NousResearch/hermes-agent#410](https://github.com/NousResearch/hermes-agent/issues/410)
- [ ] REPL TUI via **ratatui + crossterm**, codex-style (`~/src/oss/codex` is Apache-2.0, portable). Motivation: the current `src/repl/editor.rs` paint-over-raw-terminal model coordinates a `Spinner` bg thread, transient status, partial-preview, and prompt redraw by hand — wrapping rows, mid-ANSI truncation, and flush-ordering races cause lost/duplicated lines that redrawing-the-whole-frame eliminates structurally. Use ratatui's **scrolling-regions** (not alt-screen) so terminal-native scrollback and copy/paste survive. Prereq: land a `Renderer` trait + event queue on top of the current `emit_*` path so the input layer (`editor.rs`, ~2.1k LOC, termios + `libc::poll()` multiplex of stdin + relay-tunnel FD) doesn't have to move with it. Biggest risk is the relay-tunnel FD — crossterm's event stream doesn't poll arbitrary FDs, needs a `tokio::select!` shim that still pushes structured events into the web terminal (see `project_web_terminal.md`). Reusable donor abstractions: `Renderable`, `HistoryCell`, `StreamController`, `AppEvent`/`TuiEvent` enums. Est. 2–4 weeks once the render-trait prereq lands. **Defer** until the markdown-rendering fixes in `src/md.rs` prove insufficient and correctness regressions persist.

## Recently Completed

- [x] Daemon consolidation (ext-server absorbed into daemon)
- [x] Chrome extension OAuth flow
- [x] Native messaging for extension auto-connect
- [x] Cross-channel bus messaging
- [x] KV/TOTP encryption at rest
- [x] `sidekar devices` and `sidekar sessions` commands
- [x] `--verbose` flag for startup messages
- [x] Suppress Chrome automation infobar (`--test-type`)
- [x] Bus warning when not in sidekar wrapper
- [x] Move relay from Fly.io to GCP

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
