# Credits

Sidekar incorporates ideas and code ported from several open-source projects. This file records those donors for attribution when the repo is opened.

Cargo crate and npm package dependencies are not listed here — their licenses are tracked by their respective package managers (`Cargo.toml`, `www/package.json`). This file covers code that was copied or adapted into this repo.

## Ported code

### agent-browser (v1.0.21) — vercel-labs/agent-browser

Browser-automation command set ported wholesale in commit `ec95c66` (2026-04-05).

Ported features:
- `geo`, `mouse`, `state`, `auth`, `screencast` commands
- `screenshot --annotate`
- `find --role / --text / --label / --testid`
- `network` HAR export
- REPL editor refinements: word navigation, multiline editing, bracketed paste

Touches: `src/commands/core.rs`, `src/commands/data.rs`, `src/commands/interaction/pointer.rs`, `src/commands/interaction/query.rs`, `src/commands/session.rs`, `src/repl/editor.rs`.

Upstream: https://github.com/vercel-labs/agent-browser

### caveman — JuliusBrussee/caveman (MIT, © 2026 Julius Brussee)

Terse-communication prompt rules embedded into both the REPL system prompt and the PTY agent startup-inject text. Added in commit `3a5b214` (2026-04-11).

Touches: `src/repl/system_prompt.rs`, `src/agent_cli/mod.rs` (STARTUP_INJECT).

Upstream: https://github.com/JuliusBrussee/caveman

### rtk — rtk-ai/rtk (Apache-2.0)

`src/rtk.rs` — command rewrite / output-compaction toolkit. Added in commit `603b635` (2026-03-30). The `Classification { Supported, Unsupported, Ignored }` enum shape and the rewrite-rule registry mirror rtk-ai/rtk's `src/discover/registry.rs`.

Upstream: https://github.com/rtk-ai/rtk

### pakt — sriinnu/clipforge-PAKT (MIT)

`src/pakt.rs` — Rust port of the PAKT packed-document format (`@`-prefixed headers, `@k<n>` alias dict keys) for lossless compression of JSON / YAML / CSV. Added in commit `603b635` (2026-03-30).

Upstream is a TypeScript implementation (`packages/pakt-core/`) — sidekar reimplements the format in Rust rather than copying source.

Upstream: https://github.com/sriinnu/clipforge-PAKT

### memory — original

`src/memory.rs` was added in the same commit (`603b635`) but is self-contained sidekar memory storage (sqlite-backed event log, types `decision` / `convention` / `constraint` / `preference` / `open-thread` / `artifact-pointer`). No upstream identified; treat as original.

## Patterns studied (no code copied)

### openai/codex (Apache-2.0) — `codex-rs/tui/src/markdown_stream.rs`

The streaming markdown-to-ANSI pipeline in `src/md.rs` solves the same problem as codex's `markdown_stream.rs` (commit-on-boundary vs. retroactive reinterpretation). The sidekar implementation is independent — commit-at-block-boundary rather than codex's commit-per-newline-and-re-render — but the design space was informed by reading codex.

## Intentionally not a donor

### steipete/Peekaboo

An early plan was to integrate Peekaboo (Swift macOS automation CLI) for desktop automation. That plan was dropped — the functionality was rewritten in pure Rust using `objc2` + `core-foundation` + `ApplicationServices` FFI. No Peekaboo code is in this repo.

## Agents wrapped at runtime (not donors)

Sidekar wraps these agent CLIs via PTY. It invokes them as external processes and passes prompts via their native flags — their source is not incorporated into this repo.

- Claude Code (`claude`)
- OpenAI Codex CLI (`codex`)
- Cursor Agent (`cursor-agent`)
- Gemini CLI (`gemini`)
- opencode (`opencode`)
- pi-mono (`pi`)
