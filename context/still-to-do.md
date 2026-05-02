# Still to do

Consolidated gaps after removing `self-learning-v1-plan.md` and `sidekar-orchestration-plan.md`. Implemented pieces live in code (`memory/candidates.rs`, `repl/journal/`, `broker.rs`, `pty.rs`, `sidekar bus`, etc.).

## Self-learning / REPL memory loop

- **Startup retrieval**: move past fixed-count type buckets toward token-budgeted ranking; optionally fold **cwd / path hints** into ranking (today `startup_brief` uses project-scoped recency + types; per-turn uses `relevant_brief` + path-like terms).
- **Schema / provenance** (only if needed beyond usage rows): richer fields on `memory_events` or structured `detail_json` for task/path/selection stats the old plan named explicitly.
- **REPL observability**: slash or help paths for **why** a memory surfaced, **recent auto-learned** rows, etc. (`sidekar memory usage` / `memory candidates` cover some of this today).
- **Usage timing**: optionally log **`selected`** at injection time separately from **`accepted`** after a successful turn (today `accept_selected_memories` writes both together).
- **Negative signals**: first-class flow when the user **corrects** the model → contradiction / demotion wired to UX, not only candidate pipeline internals.
- **`/status`**: surface learning lifecycle snippets if useful (counts, last promotion, etc.).
- **Extractor**: optional LLM fallback for ambiguous high-signal journal slices (heuristic / journal path is primary today).
- **Maintenance**: periodic decay / compaction / stale open-thread handling beyond current hygiene passes (confirm what `memory hygiene` already covers before expanding).

## Orchestration / adapters / broker

- **Capability model**: explicit per-session capabilities (`inject_text`, `submit`, capture, focus, …) and **routing by capability**, not only by agent name / PTY vs not.
- **Terminal adapters** for sessions Sidekar did not launch: **Terminal.app**, **iTerm2**, **Warp**, **WezTerm**, **kitty**, **Ghostty** (identify session → inject → submit semantics + safety).
- **Editor adapters**: **VS Code**, **Zed**, **Cursor** (terminal-backed vs native agent UI).
- **Desktop app adapters**: **Codex app**, **Claude Desktop** where there is no PTY (APIs first, accessibility fallback).
- **Attention / UX**: badges, desktop notifications, queue visibility — broker stays source of truth; attention stays derivative (today: broker events, `monitor`, poller nudges).
- **CLI naming** (optional alignment): planned names like `sessions` / `claim` / `attach` / `watch` vs current **`agent-sessions`**, **`bus`**, **`monitor`**.

## Non-goals (unchanged)

- Cross-project speculative synthesis by default  
- Repo-wide background file import by default  
- Worker farm / always-on LLM on every micro-event  
- **A2A** as the core primitive for local session control (fine later as optional gateway)
