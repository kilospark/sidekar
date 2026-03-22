# Sidekar Orchestration Plan

`sidekar` is no longer just "web automation for an agent."

The product direction is:
- orchestration and coordination for coding agents
- multiple control adapters for CLI, terminal, editor, and desktop app surfaces
- browser automation as one adapter, not the whole product

## Core Position

`sidekar` should become a sidecar and control plane for coding agents such as:
- `codex`
- `claude`
- `gemini`
- `agent` (Cursor)
- `opencode`
- `copilot`
- `aider`

The architecture should not depend on:
- `tmux`
- browser-only workflows
- direct control of every agent process
- A2A as the core internal protocol

## Main Problem

The hard problem is not transport.

The hard problem is:
- discovering active agent sessions
- identifying how each session can be controlled
- delivering text and submit reliably
- handling uncontrolled sessions that were not launched by `sidekar`
- coordinating work across many sessions without assuming the agent is directly programmable

## Architecture

Split the system into four layers:

1. `Broker`
- durable source of truth
- tracks sessions, tasks, leases, presence, capabilities, events
- owns request/reply state and routing

2. `Attention`
- nudges and notifications
- terminal badges, `tmux`, desktop notifications, app badges, editor indicators
- should not be the primary state store

3. `Control Adapters`
- product-specific ways to reach a session
- examples: PTY wrapper, `Terminal.app`, `iTerm2`, `Warp`, `VS Code`, `Zed`, `Codex app`

4. `Automation Adapters`
- browser, desktop accessibility, editor surface introspection, terminal capture
- used to inspect state and drive UI when needed

## Shared Broker Model

The broker should be the system of record.

Minimum objects:
- `Session`
- `CapabilitySet`
- `Task`
- `Lease`
- `Presence`
- `Event`

Suggested session fields:
- `session_id`
- `agent_kind`
- `adapter_kind`
- `workspace`
- `cwd`
- `owner`
- `created_at`
- `last_seen_at`
- `status`
- `focus_required`
- `background_safe`
- `control_mode`

Suggested capability fields:
- `inject_text`
- `submit`
- `control_keys`
- `capture_output`
- `focus_window`
- `select_tab`
- `works_in_tmux`
- `native_ui`
- `terminal_ui`
- `can_launch_agent`

Suggested task operations:
- `register_session`
- `heartbeat`
- `post_task`
- `claim_task`
- `ack_task`
- `complete_task`
- `fail_task`
- `requeue_task`
- `send_input`
- `send_control`

## Adapter Strategy

Treat every way of reaching an agent as an adapter behind one model.

### 1. PTY Wrapper Adapter

Launch flow:
- `sidekar codex ...`
- `sidekar claude ...`
- `sidekar gemini ...`
- `sidekar agent ...`
- `sidekar opencode ...`

Why this matters:
- `sidekar` owns the PTY from the start
- input injection is direct PTY writing
- submit and control keys are real terminal bytes
- best reliability for CLI sessions launched through `sidekar`

Limits:
- does not help for already-running unmanaged sessions
- does not help for desktop/editor-native agent apps

Priority:
- very high

### 2. Terminal Session Adapters

Targets:
- `Terminal.app`
- `iTerm2`
- `Warp`
- `WezTerm`
- `kitty`
- `Ghostty`

Goal:
- control unmanaged terminal sessions by targeting terminal-specific sessions, tabs, panes, or ttys

Current findings:
- `Terminal.app` can inject text into the current session
- `Terminal.app` can fully submit into simple foreground processes
- `Terminal.app` can fully submit into a simple process inside `tmux`
- current `tmux` + `codex` behavior is less reliable and should be treated as partial
- synthetic keyboard events through macOS do not reliably reach `tmux`-hosted coding-agent UIs

Priority:
- high

### 3. Editor Adapters

Targets:
- `VS Code`
- `Zed`
- `Cursor`

Goal:
- target integrated terminals and native agent panels
- distinguish "terminal-backed CLI agent" from "native editor agent UI"

Priority:
- high

### 4. Desktop App Adapters

Targets:
- `Codex app`
- `Claude desktop`

Goal:
- control app-native agent surfaces where there is no terminal
- use app-specific APIs if available
- use accessibility as fallback only

Priority:
- medium to high

### 5. Browser Adapter

This remains important, but as one adapter.

Role:
- browser automation
- browser session monitoring
- browser-backed workflows triggered by tasks

Priority:
- keep current strength, but do not let it define the whole product

## Routing Model

Requests should route by capabilities, not by hardcoded product logic.

Examples:
- if session supports `inject_text` and `submit`, use direct input
- if session supports `inject_text` but not `submit`, downgrade expectations or use task queue
- if session is `focus_required`, do not background-send unless user allows it
- if session is `background_safe`, it may be used for unattended work

This lets `sidekar` choose the best available adapter without forcing a single mechanism everywhere.

## Product Direction

`sidekar` should support two broad operating modes.

### Owned Sessions

Sessions launched by `sidekar`.

Examples:
- `sidekar codex`
- `sidekar claude`
- `sidekar gemini`

Benefits:
- strongest control
- reliable input model
- simplest coordination

### Discovered Sessions

Sessions found after launch.

Examples:
- a user already running `codex` in `Terminal.app`
- a user running `claude` in `iTerm2`
- a user working in `VS Code` agent mode
- a user running `Codex app`

Benefits:
- broader coverage

Costs:
- adapter-specific targeting
- lower reliability
- more focus and safety constraints

## Why Not Make A2A The Core

A2A is useful for interoperable agent services, but it is not the core answer here.

Reason:
- many target sessions are unmanaged local UIs
- we do not control those agents directly
- the problem is local session discovery and control, not just message schema exchange

Use A2A later as:
- a gateway for external agent systems
- an optional interoperability layer

Do not use A2A as the internal primitive for local session control.

## Implementation Phases

### Phase 1: Broker and Capability Model

Build:
- durable broker
- session registry
- capability model
- task and lease model
- request/reply tracking

Likely storage:
- SQLite first

### Phase 2: PTY Wrapper

Build:
- `sidekar <agent> ...` launch wrapper
- PTY ownership
- session registration
- direct send-input API
- resize and signal forwarding
- output capture hooks

This should become the reference adapter.

### Phase 3: Terminal Adapters

Build:
- `Terminal.app` adapter
- `iTerm2` adapter
- `Warp` adapter
- `WezTerm` adapter
- `kitty` adapter
- `Ghostty` adapter

Target outcome:
- reliable session identification
- text injection
- submit behavior classification
- safety model per terminal

### Phase 4: Editor and Desktop App Adapters

Build:
- `VS Code`
- `Zed`
- `Cursor`
- `Codex app`
- `Claude desktop`

Target outcome:
- identify whether the active surface is terminal-backed or native UI
- provide the best available control path

### Phase 5: Attention and UX

Build:
- desktop notifications
- terminal/app badges
- queue visibility
- task claiming UX
- session inspection tools

Attention should help the user and the agent, but should not hold the actual state.

## Initial Command Model

Potential commands:
- `sidekar sessions`
- `sidekar tasks`
- `sidekar send`
- `sidekar claim`
- `sidekar attach`
- `sidekar watch`
- `sidekar codex ...`
- `sidekar claude ...`
- `sidekar gemini ...`

Potential internal actions:
- `send_text`
- `send_submit`
- `send_control`
- `focus`
- `capture`
- `heartbeat`

## Testing

Testing should be split into:
- terminal test matrix
- editor/app test matrix

Existing notes:
- [terminal-agent-test-template.md](/Users/karthik/src/sidekar/context/terminal-agent-test-template.md)
- [editor-agent-test-template.md](/Users/karthik/src/sidekar/context/editor-agent-test-template.md)

## Short-Term Priorities

1. finalize the broker and capability model
2. build the PTY wrapper for owned CLI sessions
3. harden terminal adapters for unmanaged sessions
4. test editor and desktop app surfaces separately
5. keep browser automation as an adapter, not the identity of the product

## Practical Rule

Design around this rule:

`sidekar` coordinates tasks centrally, but controls sessions through the best adapter available for each surface.

That keeps the orchestration model stable even as the control method varies by terminal, editor, app, or browser.
