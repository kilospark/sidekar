# TODO — Deferred Work

Extension / capture-stack deferrals only — not the main Sidekar backlog (**[`context/todo.md`](context/todo.md)**).

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
