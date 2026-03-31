---
name: sidekar
version: 0.3.0
description: |
  Agent-facing browser automation, messaging, desktop control, background jobs,
  local memory/tasks, context shaping, and encrypted local state. Use Sidekar
  when you need a real browser, cross-agent coordination, native macOS
  interaction, persistent local state, or token-aware context utilities.
allowed-tools:
  - Bash(sidekar:*)
---

# Sidekar

Sidekar is an agent utility binary. Treat it as a capability layer.

Do not guess command syntax. Use the CLI help as the source of truth:

```bash
sidekar --help
sidekar help <command>
sidekar help ext
```

If `sidekar` is missing:

```bash
which sidekar || curl -fsSL https://sidekar.dev/install | sh
```

## Capabilities

**Browser automation** — Navigate, read, click, type, screenshot in a real Chrome session.
Entry: `sidekar navigate <url>`, `sidekar read`. Explore: `sidekar help navigate`

**Extension automation** — Automate the user's normal Chrome profile via the Sidekar extension.
Entry: `sidekar ext tabs`. Explore: `sidekar help ext`

**Page perception** — Use the cheapest tool that is sufficient:
`read` → `ax-tree -i` / `observe` → `text` → `dom` → `search` / `read-urls` → `screenshot --ref=...` → `screenshot`

**Interaction** — Click, hover, fill, type, paste, keyboard, drag, scroll, upload, dialogs, wait.
Entry: `sidekar click ...`, `sidekar fill ...`. Explore: `sidekar help click`

**Browser inspection** — Console, network, cookies, storage, service workers, downloads, security.
Entry: `sidekar console`, `sidekar network`. Explore: `sidekar help console`

**Desktop automation** — Native macOS UI: find elements, click, type, screenshot, launch/quit apps.
Entry: `sidekar desktop apps`. Explore: `sidekar help desktop`

**Bus (multi-agent)** — Discover agents, send requests, hand off work, inspect open requests and replies.
Entry: `sidekar bus who`. Explore: `sidekar help bus`

**Background automation** — Monitor tabs for changes, schedule recurring jobs.
Entry: `sidekar monitor status`, `sidekar cron list`. Explore: `sidekar help monitor`, `sidekar help cron`

**Repo context** — Pack repos, summarize changes, discover and run repo actions.
Entry: `sidekar help repo`

**Memory and tasks** — Durable local memory, task lists with dependency edges, agent session history.
Entry: `sidekar help memory`, `sidekar help tasks`, `sidekar help agent-sessions`

**Context shaping** — Compact noisy output, pack/unpack structured data (JSON, YAML, CSV).
Entry: `sidekar help compact`, `sidekar help pack`

**Secrets** — Encrypted local key-value store and TOTP generation.
Entry: `sidekar kv list`, `sidekar totp list`. Explore: `sidekar help kv`, `sidekar help totp`

**Account** — Login, logout, devices, sessions, config, daemon, errors, feedback.
Entry: `sidekar login`, `sidekar help config`

## Operating Rules

1. After state-changing browser actions, read the returned brief before deciding the next step.
2. Prefer `read`, `ax-tree -i`, or `text` before taking screenshots.
3. Prefer refs from `ax-tree -i` or `observe` over brittle selectors.
4. Use `--text` matches before CSS selectors when that is simpler and reliable.
5. Use coordinates only as a last resort.
6. If login, CAPTCHA, or 2FA blocks progress, run `sidekar activate` and tell the user.
7. Never touch tabs you did not create in your session.
8. Close tabs you opened when the task is done.
9. For stale or broken web apps, inspect `storage`, `service-workers`, `cookies`, and `network` before guessing.
10. Use CLI help for exact syntax instead of inventing flags or subcommands.

## Targeting Priority

1. refs from `ax-tree -i`, `observe`, or `text`
2. `--text "..."` matches
3. CSS selectors
4. `sidekar eval ...` as an escape hatch
5. coordinates as a last resort
