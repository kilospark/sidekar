---
name: sidekar
version: 1.0.12
description: |
  Agent utility layer: inter-agent bus, encrypted secrets (KV/TOTP), durable
  memory & tasks, browser/desktop automation, scheduled jobs, repo context,
  and output compaction. Use Sidekar when you need coordination, persistence,
  real browsers, or token-efficient tooling.
allowed-tools:
  - Bash(sidekar:*)
---

# Sidekar

Sidekar is an agent utility binary. Treat it as a capability layer.

Do not guess command syntax. Use the CLI help as the source of truth:

```bash
sidekar --help
sidekar help <command>
```

If `sidekar` is missing:

```bash
which sidekar || curl -fsSL https://sidekar.dev/install | sh
```

## Capabilities

**Bus (multi-agent)** — Discover agents, send requests, hand off work, track replies.
Entry: `sidekar bus who`. Explore: `sidekar help bus`

**Secrets (KV)** — Encrypted local key-value store for tokens, API keys, credentials.
Entry: `sidekar kv list`, `sidekar kv get <key>`. Explore: `sidekar help kv`

**TOTP** — Store TOTP secrets, generate current codes for automated login flows.
Entry: `sidekar totp list`, `sidekar totp get <service> <account>`. Explore: `sidekar help totp`

**Memory** — Durable local memory scoped to project or global. Write conventions, search context, compact related memories, rate and review.
Entry: `sidekar memory context`, `sidekar memory search <query>`. Explore: `sidekar help memory`

**Tasks** — Local task list with priority and dependency edges.
Entry: `sidekar tasks list`, `sidekar tasks add "<title>"`. Explore: `sidekar help tasks`

**Agent sessions** — Inspect session history, add notes, rename sessions.
Entry: `sidekar agent-sessions`. Explore: `sidekar help agent-sessions`

**Scheduled jobs** — Cron expressions or simple intervals. Run tools, bash, or inject prompts.
Entry: `sidekar cron list`, `sidekar loop 5m "check status"`. Explore: `sidekar help cron`, `sidekar help loop`

**Repo context** — Pack repos, summarize changes, discover and run project actions.
Entry: `sidekar repo tree`, `sidekar repo changes`. Explore: `sidekar help repo`

**Context shaping** — Compact noisy output for agents. Pack/unpack structured data.
Entry: `sidekar compact run <cmd>`, `sidekar pack <file>`. Explore: `sidekar help compact`, `sidekar help pack`

**Browser automation** — Navigate, read, click, type, screenshot in a real Chrome session.
Entry: `sidekar navigate <url>`, `sidekar read`. Explore: `sidekar help navigate`
- *Perception*: `read` → `ax-tree -i` / `observe` → `text` → `dom` → `search` / `read-urls` → `screenshot`
- *Interaction*: `click`, `fill`, `type`, `keyboard`, `scroll`, `drag`, `upload`, `wait-for`
- *Inspection*: `console`, `network`, `cookies`, `storage`, `service-workers`

**Extension automation** — Automate the user's normal Chrome profile via the Sidekar extension.
Entry: `sidekar ext tabs`. Explore: `sidekar help ext`

**Desktop automation** — Native macOS UI: find elements, click, type, screenshot, launch/quit apps.
Entry: `sidekar desktop apps`. Explore: `sidekar help desktop`

**Monitor** — Watch browser tabs for background changes.
Entry: `sidekar monitor status`. Explore: `sidekar help monitor`

**Account** — Login, logout, devices, sessions, config, daemon.
Entry: `sidekar login`, `sidekar help config`

## Operating Rules

1. Use CLI help for exact syntax — never invent flags or subcommands.
2. Check `sidekar bus who` before assuming you are working alone.
3. Use `sidekar kv` for any secret or credential — never store in plain files.
4. Use `sidekar totp get` during login flows that require 2FA codes.
5. Write durable learnings to `sidekar memory write` so future sessions benefit.
6. Pipe noisy command output through `sidekar compact filter` or use `sidekar compact run`.
7. After state-changing browser actions, read the returned brief before deciding next step.
8. Prefer `read`, `ax-tree -i`, or `text` before taking screenshots.
9. Prefer refs from `ax-tree -i` or `observe` over CSS selectors; coordinates only as last resort.
10. If login, CAPTCHA, or 2FA blocks browser progress, run `sidekar activate` and tell the user.
11. Never touch browser tabs you did not create. Close tabs you opened when done.
