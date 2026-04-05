# sidekar repl — Standalone LLM Agent Mode

## Overview

`sidekar repl` turns sidekar into a standalone LLM agent. It uses sidekar's own CLI tools through a bash tool + SKILL.md — the same way Claude Code or Codex would use sidekar, but without needing an external agent harness.

## Modules

```
src/providers/
  mod.rs          — Provider enum (Anthropic/Codex), message types, model registry, verbose flag
  anthropic.rs    — Anthropic Messages API, SSE streaming, OAuth header handling, tool name casing
  codex.rs        — OpenAI Codex Responses API, SSE streaming
  oauth.rs        — PKCE OAuth for both providers, named credentials, local callback server, KV persistence

src/agent/
  mod.rs          — Agent loop: stream → extract tool calls → execute → repeat (max 25 iterations)
  tools.rs        — 6 tool definitions: bash, read, write, edit, glob, grep
  compaction.rs   — Two-phase context compaction (cheap clear + LLM summarization)

src/repl.rs       — REPL entry point, system prompt builder, slash commands, bus integration
src/session.rs    — SQLite session persistence (repl_sessions + repl_entries tables)
```

## CLI

```
sidekar repl                          # Interactive REPL (default model: claude-sonnet-4)
sidekar repl -p 'prompt'              # Single turn, exit after response
sidekar repl -m <model-id>            # Specify model
sidekar repl -r <credential-name>     # Use named credential
sidekar repl --verbose                # Dump API requests/responses to stderr

sidekar repl login <nickname>         # OAuth login (claude-1, codex-2, etc.)
sidekar repl logout <nickname|all>    # Remove credentials
sidekar repl credentials              # List stored credentials
```

## Providers

### Anthropic (Claude subscription)
- OAuth PKCE via `claude.com/cai/oauth/authorize` → `platform.claude.com/v1/oauth/token`
- Scopes: `org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload`
- After login, fetches profile from `GET /api/oauth/profile` to get `account_uuid`
- API calls include `metadata.user_id` with `account_uuid` for subscription routing
- Tool names must be PascalCase for OAuth: `Bash`, `Read`, `Write`, `Edit`, `Glob`, `Grep`
- Required headers: `anthropic-beta: claude-code-20250219,oauth-2025-04-20,...`, `user-agent: claude-cli/2.1.87`, `x-app: cli`
- System prompt must start with `"You are Claude Code, Anthropic's official CLI for Claude."`
- Fallback: `ANTHROPIC_API_KEY` env var

### OpenAI Codex (ChatGPT subscription)
- OAuth PKCE via `auth.openai.com/oauth/authorize` → `auth.openai.com/oauth/token`
- Callback: `http://localhost:1455/auth/callback`
- Extra params: `id_token_add_organizations=true`, `codex_cli_simplified_flow=true`
- JWT decode extracts `chatgpt_account_id` from `https://api.openai.com/auth` claim
- API: `POST https://chatgpt.com/backend-api/codex/responses`
- Headers: `chatgpt-account-id`, `OpenAI-Beta: responses=experimental`
- Body uses `instructions` (not `system`), `input` (not `messages`), `stream: true` required
- Tool format: `{ type: "function", name, description, parameters }`
- Tool results: `{ type: "function_call_output", call_id, output }`
- Fallback: `OPENAI_API_KEY` env var

### Named Credentials
- Nicknames like `claude-1`, `claude-2`, `codex-1` stored as `oauth:<nickname>` in KV
- Prefix determines provider: `claude-*` → Anthropic, `codex-*` → Codex

## Models

### Anthropic
- `claude-opus-4-20250514` (Opus 4, 200K context, 32K output, adaptive thinking)
- `claude-sonnet-4-20250514` (Sonnet 4, 200K context, 16K output, budget thinking)
- `claude-sonnet-4-6-20250514` (Sonnet 4.6, adaptive thinking)
- `claude-haiku-4-5-20251001` (Haiku 4.5, no thinking)

### Codex
- `gpt-5.1-codex-mini` (272K context, 128K output)
- `gpt-5.2-codex` (272K context, 128K output)
- `gpt-5.3-codex` (272K context, 128K output)
- `gpt-5.4-mini` (272K context, 128K output)

## Tools

The REPL exposes 6 tools to the LLM. Sidekar commands are accessed through `bash`:

| Tool | Purpose |
|------|---------|
| `bash` | Shell execution — sidekar CLI commands go through here |
| `read` | Read file with line numbers, offset/limit |
| `write` | Write/create file |
| `edit` | Exact string replacement in file |
| `glob` | Find files by pattern |
| `grep` | Search file contents by regex |

## Agent Loop

1. Build context: system prompt + history + user message
2. Call LLM (stream response)
3. If tool calls in response → execute tools → append results → goto 2
4. If no tool calls → done, return to user
5. Max 25 iterations per turn
6. Auto-compact at 50% of context window

## Compaction (hermes-inspired)

### Phase 1 — Cheap (no LLM call)
- Clear old `ToolResult` content with `[Cleared]` (keep last 10 messages)
- Drop old thinking blocks

### Phase 2 — LLM summarization
- Protect first 3 messages + last ~20K tokens
- Summarize middle turns with structured template:
  Goal, Constraints, Progress (Done/In Progress/Blocked), Key Decisions, Relevant Files, Next Steps, Critical Context
- Summary replaces middle messages

## Session Persistence

SQLite tables in `sidekar.sqlite3`:
- `repl_sessions` — id, cwd, model, provider, name, timestamps
- `repl_entries` — id, session_id, parent_id, entry_type, role, content (JSON), timestamp

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
