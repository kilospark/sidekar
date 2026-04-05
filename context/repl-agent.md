# sidekar repl

## Overview

`sidekar repl` runs Sidekar as a standalone LLM agent. It owns the input loop, session persistence, streaming renderer, bus registration, and slash-command UX, while the model gets a single execution tool: `bash`.

The REPL system prompt is built in `src/repl.rs` by `build_system_prompt()`. It includes:

- a concise coding-and-automation identity
- `bash` tool guidance
- Sidekar CLI capability guidance, including `sidekar skill`
- explicit rules against secrets exfiltration, destructive actions, and prompt-injection compliance
- current working directory and date
- optional startup memory from `crate::memory::startup_brief(5)`

## Main modules

```
src/repl.rs
  REPL entry point, system prompt builder, line editor, slash commands,
  bus registration, relay hookup, and stream rendering

src/agent/mod.rs
  Main agent loop: stream → handle tool calls → append results → continue

src/agent/tools.rs
  Model-visible tool definitions and execution

src/agent/compaction.rs
  Auto-compaction and manual compaction used by /compact

src/providers/
  Provider abstraction plus Anthropic, Codex, and OpenRouter backends

src/providers/oauth.rs
  Credential storage, provider detection, OAuth flows, and OpenRouter key entry

src/session.rs
  REPL session persistence and project-scoped input-history persistence
```

## Invocation

```
sidekar repl [-c <credential>] [-m <model>] [-p <prompt>] [-r [session_id]] [--verbose]
```

Current subcommands documented in `src/lib.rs`:

```bash
sidekar repl login <provider>
sidekar repl logout [name|all]
sidekar repl credentials
sidekar repl models -c <credential>
sidekar repl sessions
```

Credential prefixes determine provider:

- `claude...` → Anthropic
- `codex...` → OpenAI Codex
- `or...` → OpenRouter

Stored credentials live in KV under `oauth:<nickname>`.

## Tool surface

The REPL currently exposes exactly one model-visible tool:

- `bash` — execute a shell command and return compacted output

The model is expected to use `sidekar` through that tool. `sidekar skill` is no longer a separate tool; the prompt tells the model to run it via `bash` when it needs the command catalog.

## Runtime behavior

- Interactive mode keeps a local session history in SQLite and supports resume.
- `-p <prompt>` runs a single-turn session and exits after the response.
- The REPL registers on the local bus as `sidekar-repl`.
- If relay is enabled and the machine has a device token, the REPL can attach a tunnel for web-terminal access.
- A raw-mode mini line editor handles left/right navigation, history up/down, delete, and project-scoped persisted command history.

## Slash commands

Current slash commands are implemented in `src/repl.rs`:

- `/credential`
- `/credentials`
- `/model`
- `/models`
- `/new` and `/reset`
- `/sessions`
- `/resume`
- `/compact`
- `/verbose`
- `/quit`, `/exit`, `/q`
- `/help`

Unknown `/...` input is treated as normal prompt text so absolute paths like `/Users/.../image.png` are not hijacked as slash commands.

## Persistence

REPL state is stored in `~/.sidekar/sidekar.sqlite3`:

- `repl_sessions` stores session metadata
- `repl_entries` stores persisted message history
- `repl_input_history` stores submitted mini-line history scoped by canonical project root

The input-history store is separate from the chat transcript. It exists only to support persistent up/down history across REPL restarts in the same project.

## Notes

- `provider` in `repl_sessions` is a free-form label written by current code, not a strict provider enum.
- Top-level `sidekar help` still shows a narrower REPL synopsis than the actual parser supports. Use `src/lib.rs` and `src/main.rs` as the source of truth when changing CLI behavior.

Sessions scoped to cwd. Auto-resumes latest session. `/new` creates fresh session.

## Bus Integration

- Registers as `sidekar-repl-<pid>` with nick `self` on the bus
- Other agents can send messages via `sidekar bus send sidekar-repl-<pid> "message"`
- Incoming bus messages injected as user messages before next LLM call
- Unregisters on exit

## System Prompt

Assembled from:
1. Provider-required prefix (Claude Code identity for OAuth)
2. Agent identity + guidelines
3. SKILL.md content (sidekar CLI reference)
4. Context files (AGENTS.md, CLAUDE.md from cwd + ancestors)
5. Working directory + date

## Slash Commands (interactive mode)

| Command | Action |
|---------|--------|
| `/new` | Start fresh session |
| `/sessions` | List sessions for this directory |
| `/resume` | Switch to a different session |
| `/model` | Show available models + auth status |
| `/quit` | Exit REPL |
| `/help` | Show help |

## Known Limitations

- No Ctrl+C interrupt (kills process, but session is saved)
- Single-line input only (no multiline paste)
- SHA-256 for PKCE uses `openssl` subprocess
- No bash command safety checks (LLM can execute anything)
- OAuth tokens stored unencrypted in KV (encrypted only when logged in to sidekar.dev)

## Reference Implementations Studied

- **pi-mono** (primary): Agent loop, provider OAuth, skills, sessions, TUI
- **hermes-agent**: Compaction algorithm, structured summary template
- **free-code**: Claude Code internals — OAuth headers, metadata, tool name casing, system prompt requirements
- **anything-llm**: Provider abstraction patterns
- **claw42** (user's own): Rust agent loop architecture
