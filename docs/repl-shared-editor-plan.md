# REPL Shared Editor Rewrite Plan

## Goal

Replace the current hand-wired one-surface mini-line with a shared editor model that:

- keeps one live draft for both local terminal and browser tunnel viewers
- mirrors every keypress and cursor move across both surfaces
- renders correctly with wrapped input, wide Unicode, and combining characters
- preserves the richer Sidekar REPL flow already present in `src/repl.rs`
- treats browser width and local terminal width as separate layout inputs

This is not a full-screen TUI rewrite. The REPL remains a streaming terminal app with an editable prompt at the bottom.

## Non-goals

- no migration to `ratatui`, `reedline`, or `rustyline-async`
- no change to the high-level REPL command set or session model
- no attempt to share viewport scroll position between browser and local terminal

## Current Problems

The current editor in [`src/repl.rs`](/Users/karthik/src/sidekar-repl-editor/src/repl.rs) mixes three concerns:

- input parsing
- editor state
- rendering/output

That creates the current failure modes:

- wrapped redraw bugs
- bad cursor math for wide and combining Unicode
- tunnel input only joins on submit instead of sharing the live draft
- browser resize is cosmetic in `www/public/js/terminal.js` and does not inform the REPL editor
- prompt redraw and shared output are coupled because the current prompt writer also calls `tunnel_send`

## Target Architecture

### 1. Editor Core

Create `src/repl/editor.rs` with a shared, surface-agnostic editor core.

Primary type:

- `EditorState`

Fields:

- `buffer: String`
- `cursor: usize` as a byte offset on grapheme boundaries
- `preferred_col: Option<usize>` for visual up/down movement
- `history: Vec<String>`
- `history_index: Option<usize>`
- `history_draft: Option<String>`
- `kill_buffer: String`
- `pending_escape` and `pending_utf8` state if we keep byte-level parsing in the editor adapter

Core responsibilities:

- insert text
- backspace/delete
- left/right by grapheme
- up/down by visual display column
- home/end
- `Ctrl+A`, `Ctrl+E`, `Ctrl+U`, `Ctrl+K`, `Ctrl+Y`
- history prev/next
- submit / clear / EOF semantics

Unicode/layout primitives should be adapted from Codex donor code:

- grapheme traversal from `unicode-segmentation`
- display width from `unicode-width`
- wrapped-line calculation and visual cursor positioning modeled after `codex-rs/tui/src/bottom_pane/textarea.rs`

### 2. Surface Renderers

Create a renderer layer that takes `EditorState` plus a width and renders prompt state for one surface.

Primary types:

- `PromptSurfaceState`
- `PromptRenderer`

Per-surface state:

- `width`
- `rendered_rows`
- `prompt_prefix_width`
- `visible`

There are two independent surfaces:

- local terminal surface
- tunnel/browser surface

The same `EditorState` is rendered twice, once for each width. Surface row counts must not be shared.

### 3. Separate Prompt Output From Shared Output

Current prompt output is coupled to tunnel mirroring. That must be split.

Introduce separate output paths:

- shared output: model/tool/bus/slash-command lines that should go to both local and tunnel
- local prompt output: prompt redraw bytes for stdout only
- tunnel prompt output: prompt redraw bytes for tunnel only

This avoids corrupting the browser prompt when the local prompt redraws, and vice versa.

### 4. Structured Tunnel Bridge

Replace the current tunnel pipe bridge that only forwards raw bytes with a structured bridge.

New tunnel-side event model:

- `TunnelEvent::Data(Vec<u8>)`
- `TunnelEvent::ViewerResize { cols: u16, rows: u16 }`
- existing bus relay/plain/disconnect events

The REPL bridge layer should drain tunnel events into an internal queue plus a wake fd for the synchronous poll loop.

Bridge event enum inside REPL:

- `BridgeEvent::TunnelInput(Vec<u8>)`
- `BridgeEvent::TunnelResize { cols, rows }`
- `BridgeEvent::Bus`
- `BridgeEvent::Disconnected`

The poll loop wakes on:

- local stdin readable
- bridge wake fd readable
- timeout for pending escape resolution

### 5. Browser Resize Protocol

The browser must send its own geometry upstream without affecting the host terminal.

Required protocol changes:

1. `www/public/js/terminal.js`
   - send a text control message on browser/xterm resize
   - proposed payload:
     `{"type":"viewer","v":1,"event":"resize","cols":<n>,"rows":<n>}`

2. `relay/src/bridge.rs`
   - stop dropping viewer text frames
   - forward recognized viewer control JSON to the tunnel

3. `src/tunnel.rs`
   - parse viewer control JSON
   - emit `TunnelEvent::ViewerResize`

4. `src/repl.rs`
   - update tunnel renderer width only
   - do not call `send_terminal_resize()` for browser-only resize

This preserves the rule:

- browser resize changes browser prompt layout
- local terminal resize changes local prompt layout
- neither width overwrites the other

### 6. Local Terminal Width Handling

The local width should come from the real terminal, independent of browser width.

Implementation choice:

- query local terminal width during redraw and on `SIGWINCH`-adjacent wakeups
- if explicit signal wiring is unnecessary, re-query width before each redraw and before output replay

This is simpler and sufficient for a bottom-line prompt renderer.

### 7. Event Renderer Integration

`EventRenderer` and all shared printing code must become prompt-aware.

Before shared output:

- clear both prompt surfaces if visible

After shared output:

- redraw both prompt surfaces from the shared `EditorState`

Affected callers:

- stream event rendering
- slash-command informational output
- bus message display
- shell command status lines
- relay state/status lines

This gives stable behavior while streaming output above an active prompt.

## Module/File Plan

### `src/repl.rs`

Keep as orchestration layer only:

- REPL loop
- provider/session/command flow
- bridge setup/teardown
- stream callbacks

Remove editor-specific logic from this file over time.

### `src/repl/editor.rs`

New shared editor core and rendering module.

Expected contents:

- `EditorState`
- `PromptSurface`
- `PromptRenderer`
- `EditorAction`
- `EditorResult`
- width/grapheme helpers
- editor tests

### `src/tunnel.rs`

Add viewer-control parsing and a resize event for browser geometry.

### `relay/src/bridge.rs`

Forward viewer control text frames instead of dropping them.

### `www/public/js/terminal.js`

Emit browser resize control messages.

## Implementation Sequence

### Step 1. Stabilize Baseline

- start from the reconciled `src/repl.rs`
- ensure `cargo check` passes in the worktree
- keep the current rich slash-command/session behavior intact

### Step 2. Extract Editor Module

- move current `LineEditor` and related prompt code into `src/repl/editor.rs`
- preserve current behavior first
- introduce explicit local-only and tunnel-only prompt output helpers

Exit criteria:

- no behavior change
- code compiles with editor logic living outside `src/repl.rs`

### Step 3. Replace Cursor/Wrap Math

- port Codex-style grapheme-aware movement and width-aware wrapping
- add `preferred_col`
- replace `chars().count()` prompt math

Exit criteria:

- wrapped redraw bug gone
- wide/combining Unicode cursoring and wrap layout stable

### Step 4. Shared Draft Rendering

- stop mirroring prompt redraw through `tunnel_send` blindly
- render local and tunnel prompts separately from the same `EditorState`
- keep model output shared

Exit criteria:

- local typing updates both surfaces
- tunnel typing updates both surfaces
- output lines do not corrupt either prompt

### Step 5. Structured Tunnel Bridge

- replace raw tunnel pipe bytes with a bridge event queue
- add browser resize propagation

Exit criteria:

- tunnel keypresses still work
- browser resize changes only browser prompt layout

### Step 6. Output Reconciliation

- make all output-producing paths prompt-aware
- clear/redraw surfaces around stream, bus, slash-command, and shell output

Exit criteria:

- no prompt smear during streaming or command output

### Step 7. Paste and Escape Hardening

- preserve bracketed paste
- keep existing escape variants
- optionally add a bounded paste-burst heuristic later if needed

Exit criteria:

- local and tunnel paste do not submit early or corrupt the draft

## Test Plan

### Editor unit tests

Port/adapt the highest-value Codex textarea tests:

- wrapped cursor positioning
- visual up/down navigation
- wide grapheme wrapping
- combining-mark movement
- delete/backspace around grapheme boundaries
- home/end on wrapped lines
- kill/yank controls

### Renderer tests

- prompt row count for widths 20/40/80
- redraw after shared output
- independent local and tunnel widths

### Bridge tests

- tunnel data events mutate the shared draft
- tunnel resize updates browser width only
- bus/plain events do not corrupt prompt state

### End-to-end checks

- type locally, see browser mirror
- type in browser, see local mirror
- resize browser only, prompt reflows there only
- resize local terminal only, local prompt reflows without changing browser width

## Risks

### Risk: prompt/output coupling remains implicit

Mitigation:

- introduce explicit prompt renderer APIs before adding new behavior

### Risk: tunnel/browser resize protocol becomes mixed with PTY resize

Mitigation:

- use a distinct viewer control message and a distinct `TunnelEvent`

### Risk: porting too much of Codex at once

Mitigation:

- port the editor kernel concepts only
- do not copy `ratatui` or `ChatComposer`

## Immediate Next Coding Tasks

1. Add `src/repl/editor.rs` and move the current `LineEditor`, `LineEditResult`, `read_input_or_bus` helpers, and prompt redraw logic there.
2. Split prompt output into local-only, tunnel-only, and shared-output sinks.
3. Replace current prompt redraw math with width-aware wrapped layout.
4. Add the browser resize control path across `terminal.js`, `relay/src/bridge.rs`, and `src/tunnel.rs`.

