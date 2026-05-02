# TODO

Engineering backlog for Sidekar (single file). Roadmap prose from retired `context/*-plan.md` files is folded into **Self-learning** and **Orchestration** below; extension/interceptor deferrals are under **Extension / capture deferrals** near the end.

## Self-learning / REPL memory loop (remaining)

- Startup retrieval: token-budget ranking (not only fixed-count buckets); optional cwd/path-aware ranking (`startup_brief` today is project-scoped recency + types; per-turn uses `relevant_brief` + path-like terms).
- Schema/provenance only if usage rows are insufficient: richer `memory_events` or structured `detail_json` for task/path/selection stats.
- REPL observability: slash or help for why a memory surfaced, recent auto-learned rows, etc. (`sidekar memory usage`, `memory candidates` partially cover this).
- Usage timing: optionally log `selected` at injection separately from post-turn `accepted` (today `accept_selected_memories` logs both together).
- Negative signals: user correction → contradiction/demotion surfaced in UX, not only inside candidate promotion.
- `/status`: optional learning lifecycle snippets (counts, last promotion).
- Extractor: optional LLM fallback for ambiguous high-signal journal slices.
- Maintenance: decay/compaction/stale open-thread passes beyond current `memory hygiene` (confirm overlap before expanding).

## Orchestration / adapters / broker (remaining)

- Capability model: explicit per-session capabilities (`inject_text`, `submit`, capture, focus, …) and routing by capability, not only agent name / PTY vs non-PTY.
- Terminal adapters for sessions Sidekar did not launch: Terminal.app, iTerm2, Warp, WezTerm, kitty, Ghostty (session ID → inject → submit + safety).
- Editor adapters: VS Code, Zed, Cursor (terminal-backed vs native agent UI).
- Desktop app adapters: Codex app, Claude Desktop where there is no PTY (APIs first, accessibility fallback).
- Attention/UX: badges, notifications, queue visibility (broker stays source of truth; today: broker events, `monitor`, poller nudges).
- CLI naming (optional): align names like `sessions` / `claim` / `attach` / `watch` with current `agent-sessions`, `bus`, `monitor`.

### Non-goals

- Cross-project speculative synthesis by default; repo-wide background file import by default; worker farm / always-on LLM on every micro-event; **A2A** as core primitive for local control (optional gateway later is fine).

## High Priority

- [ ] Token usage tracking: side-by-side comparison of Claude, Codex, Sidekar consumption
- [ ] REPL `/status` & session stats: track usage **per credential × model** within one REPL session (today switching `/credential` / `/model` keeps one blended `TurnStats` bucket; `repl_sessions` stores opening cred/model only).
- [x] **Session journaling** — shipped on branch `journaling` (10 commits). Background idle-triggered LLM summarization, 12-section structured JSON in `session_journals`, resume injection with reference-only framing, memory promoter at threshold 3. Design doc: `context/journaling.md`. 119 new tests (407 total).
- [x] Evaluate adding sidekar as a native tool (avoid skills/SKILL.md ceremony) — REPL ships a native `Sidekar` tool with embedded catalog + operating rules (`src/agent/tools.rs:242`)
- [x] Evaluate adding edit-file and other precision tools (read, grep, glob) — REPL has Read/Write/Edit/Glob/Grep native (`src/agent/tools.rs`)
- [x] Evaluate mempalace integration (https://github.com/milla-jovovich/mempalace) — no major benefits; sidekar already covers dedup, FTS, confidence, supersession
- [x] Persona system for agents — REPL reads `AGENTS.md` from cwd, appends to system prompt (`src/repl/system_prompt.rs`)
- [x] Skills system (agent-defined capabilities) — `/skill <name>` loads SKILL.md from standard agent dirs (claude/codex/gemini/pi/opencode), session-scoped (`src/repl/skills.rs`, `src/repl/slash.rs`)
- [ ] Multi-agent orchestration (see **Orchestration** section above)
- [ ] Test inter-agent communication across machines
- [ ] Marketing strategy: public launch vs private/invite-only
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

- [x] **Port CUA Driver's background desktop automation into sidekar** (`~/src/oss/cua/libs/cua-driver`, MIT). CUA Driver (Swift, macOS 14+) drives native apps **without stealing focus/cursor** — the critical capability sidekar's `src/desktop/` module lacks. Core techniques to port to Rust FFI:
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
- [x] Evaluate [agent-desktop](https://github.com/lahfir/agent-desktop) (`~/src/oss/agent-desktop`) for ref system and skeleton traversal patterns — ported `@e1` ref system (refs.rs). Refs assigned during `find`, resolved during `click @eN`. Skeleton traversal deferred (current tree walk is adequate for now; token reduction matters more for LLM tool calls than CLI output).
- [ ] [vercel-labs/wterm](https://github.com/vercel-labs/wterm) — DOM-rendered terminal emulator (Zig→WASM), potential xterm.js replacement for web terminal
- [ ] [NousResearch/hermes-agent-self-evolution](https://github.com/NousResearch/hermes-agent-self-evolution) — DSPy+GEPA evolutionary optimization of skills/prompts; concept only (no license, Python); revisit if/when REPL prompt regression evals exist
- [ ] SKILL.md `requires_secrets` frontmatter — parse YAML frontmatter on `/skill <name>` load, check kv via new `kv_exists` (EXISTS query, no decrypt), fail-closed on missing required keys with actionable `sidekar kv set` hints; strip frontmatter from body before injecting into system prompt; values never enter agent context (skill body documents `sidekar kv exec --keys=...` shape). Files: `src/repl/skills.rs`, `src/repl/slash.rs:463-494`, `src/broker/kv_store.rs`. ~200 LOC + tests. Inspired by [NousResearch/hermes-agent#410](https://github.com/NousResearch/hermes-agent/issues/410)
- [ ] REPL TUI via **ratatui + crossterm**, codex-style (`~/src/oss/codex` is Apache-2.0, portable). Motivation: the current `src/repl/editor.rs` paint-over-raw-terminal model coordinates a `Spinner` bg thread, transient status, partial-preview, and prompt redraw by hand — wrapping rows, mid-ANSI truncation, and flush-ordering races cause lost/duplicated lines that redrawing-the-whole-frame eliminates structurally. Use ratatui's **scrolling-regions** (not alt-screen) so terminal-native scrollback and copy/paste survive. Prereq: land a `Renderer` trait + event queue on top of the current `emit_*` path so the input layer (`editor.rs`, ~2.1k LOC, termios + `libc::poll()` multiplex of stdin + relay-tunnel FD) doesn't have to move with it. Biggest risk is the relay-tunnel FD — crossterm's event stream doesn't poll arbitrary FDs, needs a `tokio::select!` shim that still pushes structured events into the web terminal (see `project_web_terminal.md`). Reusable donor abstractions: `Renderable`, `HistoryCell`, `StreamController`, `AppEvent`/`TuiEvent` enums. Est. 2–4 weeks once the render-trait prereq lands. **Defer** until the markdown-rendering fixes in `src/md.rs` prove insufficient and correctness regressions persist.


## Extension / capture deferrals (Interceptor comparison)

Features evaluated and explicitly deferred. Each entry links back to the comparison against Interceptor (github.com/Hacker-Valley-Media/Interceptor) that produced it.

---

## Request override by URL pattern

**Status:** deferred. Additive on top of topic-1 MAIN-world monkey patch.

**What it is:** Rewrite outgoing request URLs (query params, headers) before `fetch`/`XHR` sends them. Pattern-based matching with glob-style asterisks.

**Why deferred:** Topic 1 delivers capture + stealth, which is the headline value. Override is modification — separate capability, different use case (pagination, auth header injection, A/B variant forcing). Land topic 1 first, add override when agents ask for it.

**Scope:**
- Extension: ~30 LOC added to `inject-net.js` — `matchesPattern()` + `applyOverrides()` called inside patched fetch/XHR.open.
- Rust CLI: `sidekar override "*api/search*" limit=100`, `sidekar override clear`, `sidekar override list`. ~80 LOC.
- Enhancement over Interceptor: also support header overrides (Interceptor is query-params-only). ~30 LOC more.

**Reference implementation:** `~/src/oss/Interceptor/extension/src/inject-net.ts:16-51` (matcher + rewriter), `cli/commands/override.ts` (CLI shape).

**Prereq:** topic-1 MAIN-world substrate must be live.

---

## Scene-graph resolvers

**Status:** deferred entirely.

**What it is:** Per-host extension modules that make canvas-based editors (Canva, Google Docs, Google Slides, Figma, Excalidraw) addressable with semantic refs instead of pixel coordinates.

**Why deferred:** Real UX win for design/doc automation, but narrow use case. Sidekar's current `--os` CGEvent fallback + screenshots works for these apps, just less elegantly. Revisit when an agent workflow hits the wall.

**Scope when revived:**
- Extension: `extension/scene/engine.js` (~200 LOC), profiles for `google-docs.js`, `google-slides.js`, `generic.js` (~150 LOC each).
- Ref registry with `WeakRef` + signature recovery (`[tagName, role, pageId, aria-label, bbox]`).
- Rust CLI: `src/commands/scene.rs`, commands `scene profile|list|click|dblclick|text|insert|slide|render`. ~300 LOC.

**Reference implementation:** `~/src/oss/Interceptor/extension/src/content/scene/`.

**Key risk:** selectors rot when Google/Canva redesigns. Need a `sidekar scene profile --verbose` self-test command baked in from day one.

---

## Expanded macOS desktop surface (Phase 2)

**Status:** partial. Phase 2 shipped `desktop trust`, `desktop clipboard`, `desktop menu`, and CGEventTap monitor (foreground `desktop monitor watch` mode only — daemon-mode blocked on TCC, see LaunchAgent entry below). OCR and NLP entities deferred.

### Skipped in phase 1

**Speech recognition (SFSpeechRecognizer / AVSpeechSynthesizer)**
- Rust FFI is hard — Obj-C delegate callbacks, async recognitionTask.
- Cross-platform alternative: `whisper.cpp` Rust bindings. Probably the right path instead of macOS-only.
- Decision: revisit as a cross-platform feature, not a macOS-specific port.

**Sound classification (SoundAnalysis / SNClassifySoundRequest)**
- Stream-based with `SNResultsObserving` delegate — hard FFI.
- Unclear agent use case — what does classifying "water running" vs "dog barking" do for an agent?
- Decision: skip unless a concrete use case emerges.

**Audio capture (ScreenCaptureKit audio / AVCaptureSession)**
- Async Swift API, not yet in objc2. Cross-platform alternative: `cpal` crate.
- Decision: revisit as a cross-platform feature.

**Face / body / hand pose / saliency (Vision framework)**
- `VNDetectFaceRectanglesRequest`, `VNDetectHumanBodyPoseRequest`, `VNDetectHumanHandPoseRequest`, `VNGenerateAttentionBasedSaliencyImageRequest`.
- objc2 bindings feasible (same pattern as OCR which we are shipping).
- No clear agent use case yet. Skip until one emerges.

**OCR (Vision framework, VNRecognizeTextRequest)**
- Moved out of phase-2 scope 2026-04-18 per user decision.
- Still feasible via objc2 bindings. Revisit when an agent workflow needs screen text extraction that goes beyond what `read`/`text`/`ax-tree` on the browser side provide.
- Note: on macOS, `screencapture -c` + pbpaste doesn't OCR. Integration requires raw Vision framework calls.

**NLP entity extraction (NaturalLanguage, NLTagger .nameType)**
- Moved out of phase-2 scope 2026-04-18 per user decision.
- Narrow, one-off utility. Agents can route text through Claude for NER today; a local extractor only matters if volume or latency demands it.
- objc2 bindings remain feasible. ~100 LOC if revived.

**Apple Intelligence (FoundationModels.LanguageModelSession)**
- macOS 26+ only. Brand-new Swift-only async API. Not exposed via C headers. Very hard to FFI.
- Sidekar `repl` already provides Claude-backed inference.
- Decision: **permanently skip.** Sidekar's own agent loop is the right path, not piggybacking on Apple Intelligence.

**Virtual displays (CGVirtualDisplay)**
- **Private API**, no public FFI surface. Would require Swift sidecar to access — violates sidekar's "pure Rust, no Swift sidecar" invariant.
- Decision: **permanently skip.**

**HealthKit**
- Unavailable on Mac standalone (requires iPhone via iCloud sync).
- Decision: **permanently skip.**

**SensitiveContentAnalysis (SCSensitivityAnalyzer)**
- Niche. Interceptor's own implementation is a stub.
- Decision: skip unless a use case emerges.

**Notifications (DistributedNotificationCenter tap)**
- Easy FFI (~60 LOC), but the event stream is extremely noisy — every system notification from every app.
- Decision: skip unless a specific filtering use case emerges.

**Files watch (FSEvents / kqueue)**
- Easy via `notify` crate — already cross-platform, no FFI needed.
- Decision: add if an agent needs filesystem watching. Low priority; agents can poll.

### Reference

All implementations in `~/src/oss/Interceptor/interceptor-bridge/Sources/Domains/`. Each domain is ~100–400 lines of Swift — a good reference even when the Rust port path diverges.

---

## First-class Vertex AI provider

**Status:** deferred. v3.2.8 ships a Vertex adapter inside the OpenAI-compat provider type — works but clunky (manual base URL paste, manual `gcloud print-access-token`, no project picker, model IDs surface as `<publisher>/<id>`).

**What "first-class" means:**
- Own entry in provider picker (alongside Anthropic, OpenAI, Gemini, Codex, OpenRouter, OpenAI-compat). Shortcut: `ver` or `vertex`.
- Settings UI fields: GCP project, region (default `global`), publisher allowlist. No URL pasting.
- Auth: Application Default Credentials (`~/.config/gcloud/application_default_credentials.json`) with automatic token refresh. Service account JSON as alternative. Falls back to `gcloud auth print-access-token` shell-out only if ADC missing.
- Token cache + refresh-before-expiry (ya29 tokens last 1h). Reuse the OAuth refresh pattern from `src/providers/oauth.rs`.
- Model picker shows clean IDs (`gemini-2.5-pro`, `claude-sonnet-4@20250514`, `qwen3-coder-480b`) grouped by publisher. Internal mapping handles the `<publisher>/<id>` form Vertex's openapi compat requires.
- Model Garden enablement check: when a publisher returns 0 models, surface a hint linking to `https://console.cloud.google.com/vertex-ai/model-garden` with the project pre-selected.
- Region-aware: some models are region-pinned (Claude on Vertex requires `us-east5`/`europe-west1`, not `global`). Provider should know the per-model region constraint and route accordingly, or surface the constraint in the picker.

**Why deferred:** v3.2.8 unblocks the immediate need (Vertex chat works via OpenAI-compat with the auto-detection adapter). Promoting to first-class is ~400 LOC + ADC integration + UI work — worth doing if Vertex becomes a regular driver, not for one-off use.

**Reference:** existing adapter logic in `src/providers/vertex.rs` (publisher discovery, project extraction, header injection) — most of it carries over; the new work is auth, settings UI, and the provider-trait wrapper.

**Prereq when revived:**
- Decide ADC discovery order (env `GOOGLE_APPLICATION_CREDENTIALS` → file path → metadata server) and document in `context/release-cycle.md` style doc.
- Pick the canonical region default for the picker (`global` works for Gemini; Claude needs `us-east5`).

---

## Cross-cutting

### macOS code-signing + notarization pipeline

**Status:** not started. Gates durable TCC grants for distributed users and unlocks LaunchAgent-based daemon.

**Today:** binaries are ad-hoc signed (`codesign --force --sign -`) on each install. cdhash changes per build → TCC treats every build as a new identity → users (and the dev loop) re-grant Accessibility every rebuild. Distribution integrity is covered by minisign but Gatekeeper/TCC is not.

**Target:** Apple Developer ID signing + notarization + stapling, same pattern as Interceptor (`~/src/oss/Interceptor/scripts/release-dmg.sh`, `release-bridge.sh`, `install-bridge.sh`).

**Concrete steps when ready:**
- Apple Developer Program membership ($99/yr)
- Create Developer ID Application certificate; store securely in CI secrets
- Port Interceptor's `release-dmg.sh` pattern into `.github/workflows/release.yml`:
  - Sign CLI + daemon + (future) bridge separately with own bundle IDs
  - `--options runtime --timestamp` with entitlements plist (`allow-jit`, `allow-unsigned-executable-memory`, `disable-library-validation`)
  - `xcrun notarytool submit` via keychain profile
  - `xcrun stapler staple`
- Update `install.sh` to verify expected authority + team ID before copying
- Keep minisign alongside for distribution integrity (orthogonal)

**Dev-loop workaround (already available, document in CONTRIBUTING):** create a self-signed code-signing certificate in Keychain Access (`sidekar-dev`, Code Signing, Always Trust), then sign local builds with `codesign --force --sign sidekar-dev`. Stable identity = TCC grant persists across rebuilds. Single one-time grant for the dev cert.

### LaunchAgent-based daemon

**Status:** not started. Gated on Developer ID above (launchd-managed daemons work with ad-hoc too, but signing pays off together).

**Problem seen 2026-04-18:** sidekar's daemon is spawned via `Command::new(exe).arg("daemon").arg("start").spawn()` — child inherits the terminal's TCC context, then gets reparented to launchd when the CLI exits. macOS re-evaluates TCC on the orphaned process and denies Accessibility grants that the user explicitly granted to the binary path. `AXIsProcessTrusted()` returns `false` in the daemon even when it's `true` in the CLI. This blocks every daemon-side feature that needs AX: CGEventTap global monitor, window-movement helpers, future screen-capture-in-daemon paths.

**Target:** launchd-managed daemon via a proper plist, same as Interceptor's `com.interceptor.bridge.plist` installed to `~/Library/LaunchAgents/`.

**Concrete steps:**
- Ship `com.sidekar.daemon.plist` in the repo
- `install.sh`: `launchctl bootstrap gui/$(id -u) com.sidekar.daemon.plist`
- Replace `ensure_running()` spawn path with `launchctl kickstart gui/$(id -u)/com.sidekar.daemon`
- Daemon lifecycle becomes launchd's responsibility (auto-restart, log routing)
- Users grant Accessibility once to the launchd-hosted process; grant persists

**Interim:** features that need daemon-side AX (currently only CGEventTap monitor) use the foreground-CLI path (`sidekar desktop monitor watch`). Foreground mode keeps the user-interactive TCC context that works today.

### `--os` click/type/press stabilization

**Status:** click has `--os` flag (experimental). type and press do not yet.

**Open items:**
- `--os` currently on `click` only. Extend to `type` and `press` following the same `os_click_css` pattern.
- Coordinate calibration is an empirical finding, not a root-caused invariant — per tanager 2026-04-18: "keep current macOS behavior behind explicit note/guard, but mark provisional." On macOS + enigo 0.6.1 + Chrome, enigo `click_at(x, y)` lands at CSS viewport `(x, y)` of the focused window, not raw screen. Calibration layer in `src/browser/os_click.rs` currently passes CSS coords directly. Linux/Windows behavior untested.
- `sidekar debug click-probe` exists to pin down the coord model — use it on each new platform before enabling `--os` there.
- Anti-bot test matrix (BrowserScan / CreepJS / Fingerprint.com / Sannysoft / Pixelscan) deferred until `--os` is stable across click/type/press. Don't publish anti-bot claims before this.

### `sidekar activate` multi-Chrome-PID edge case

**Status:** PID-based activation landed (`src/utils.rs::activate_browser_by_pid`), but Chrome launched with `--test-type` does not reliably come to foreground even when System Events targets the right PID. Likely interacts with the LaunchAgent daemon work above. Revisit once the daemon is launchd-hosted; Chrome foregrounding from a launchd-hosted process may behave differently.

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
