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

Sidekar is an agent utility binary. Treat it as a capability layer, not as product marketing.

Use it for:

- browser automation in a real Chrome session
- automation of your normal Chrome profile through the extension
- agent-to-agent messaging and handoff
- native macOS UI automation
- background monitoring and scheduled jobs
- local memory, task tracking, repo context, and dependency management
- output compaction and structured packing for agent context
- encrypted local secrets and TOTP generation

## First Step

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

## Capability Map

### Browser Automation

Use these for real-browser work on web pages:

```bash
sidekar navigate <url>
sidekar read
sidekar ax-tree -i
sidekar click ...
sidekar type ...
```

Sidekar also has browser/session control:

```bash
sidekar launch
sidekar connect
sidekar tabs
sidekar tab <id>
sidekar new-tab [url]
sidekar close
sidekar frame <id|sel>
sidekar activate
```

Check detailed syntax with:

```bash
sidekar help navigate
sidekar help click
sidekar help new-tab
sidekar help frame
```

### Page Reading And Perception

Use the cheapest tool that is sufficient:

1. `sidekar read`
2. `sidekar ax-tree -i` or `sidekar observe`
3. `sidekar text`
4. `sidekar dom`
5. `sidekar search`, `sidekar read-urls`, or `sidekar resolve`
6. `sidekar screenshot --ref=...`
7. `sidekar screenshot`

Useful page-reading commands:

```bash
sidekar read
sidekar text
sidekar dom
sidekar ax-tree -i
sidekar observe
sidekar screenshot
sidekar pdf
sidekar search <query>
sidekar read-urls <url1> <url2>
```

### Interaction And Page Control

For filling forms, editors, dialogs, and dynamic apps:

```bash
sidekar click ...
sidekar hover ...
sidekar fill ...
sidekar keyboard ...
sidekar paste ...
sidekar insert-text ...
sidekar wait-for ...
sidekar wait-for-nav
sidekar eval ...
```

Other useful interaction surfaces exist for `select`, `upload`, `drag`, `dialog`, `press`, `scroll`, `media`, `animations`, `zoom`, `lock`, and `unlock`. Use `sidekar help <command>` for exact syntax.

### Browser Inspection And State

Use these when debugging application state or browser behavior:

```bash
sidekar console ...
sidekar network ...
sidekar cookies ...
sidekar storage ...
sidekar service-workers ...
sidekar download ...
sidekar viewport ...
sidekar block ...
sidekar security ...
```

These are useful for stale app state, service worker issues, request debugging, downloads, and certificate handling.

### Extension Automation

Use `sidekar ext ...` to automate your normal Chrome profile instead of a Sidekar-launched browser.

Start with:

```bash
sidekar ext tabs
sidekar help ext
```

Representative extension tasks:

```bash
sidekar ext read <tab_id>
sidekar ext click ...
sidekar ext type ...
sidekar ext paste ...
sidekar ext set-value ...
sidekar ext ax-tree ...
sidekar ext eval-page ...
sidekar ext new-tab
```

Always use `sidekar help ext` for the current subcommands.

### Bus And Multi-Agent Coordination

Use the bus to discover agents, send requests, and hand off work:

```bash
sidekar bus who
sidekar bus send <to> <message>
sidekar bus done <next> <summary> <request>
```

If you need agent names first, run `sidekar bus who`.

### Desktop Automation

Use desktop automation for native dialogs, app chrome, file pickers, and surfaces outside the browser.

Common commands:

```bash
sidekar desktop apps
sidekar desktop windows --app <name>
sidekar desktop find --app <name> <query>
sidekar desktop click --app <name> <query>
sidekar desktop screenshot
sidekar desktop launch <app>
sidekar desktop activate --app <name>
sidekar desktop quit --app <name>
```

### Background Automation

Use these for persistent or reactive workflows:

```bash
sidekar monitor start ...
sidekar monitor status
sidekar monitor stop

sidekar cron create ...
sidekar cron list
sidekar cron delete <job-id>
```

`monitor` is for watching tabs for changes. `cron` is for scheduled automation.

### Repo Context

Use this when you need repo-wide understanding instead of reading files one by one:

```bash
sidekar repo tree
sidekar repo pack
sidekar repo pack --style=json
```

Use it for:

- quick repo navigation with token-aware tree output
- packing a whole repo or selected files into one agent-friendly artifact
- narrowing repo context with `--include`, `--ignore`, or `--stdin`
- optionally adding `git diff` and recent `git log` context

Do not guess subcommands. Use:

```bash
sidekar help repo
```

### Memory, Tasks, And Context

Use these when the job needs durable local state, dependency tracking, or smaller context:

```bash
sidekar repo ...
sidekar memory ...
sidekar tasks ...
sidekar compact ...
sidekar pack ...
sidekar unpack ...
```

Use them for:

- storing and recalling durable project memory
- keeping a local task list with dependency edges
- packing local repositories into a single agent-readable snapshot
- shrinking noisy command output before it reaches the agent
- packing structured JSON, YAML, or CSV into a more compact transferable form

Do not guess subcommands. Use:

```bash
sidekar help repo
sidekar help memory
sidekar help tasks
sidekar help compact
sidekar help pack
sidekar help unpack
```

### Secrets And Local State

Use Sidekar for encrypted local values and TOTP codes:

```bash
sidekar kv set <key> <value>
sidekar kv get <key>
sidekar kv list
sidekar kv delete <key>

sidekar totp add <service> <account> <secret>
sidekar totp list
sidekar totp get <service> <account>
sidekar totp remove <id>
```

### Account And Environment

These are less common during task execution, but they matter when auth or device state is involved:

```bash
sidekar login
sidekar logout
sidekar devices
sidekar sessions
sidekar config ...
sidekar daemon ...
sidekar errors
sidekar feedback ...
```

Use `sidekar feedback ...` when Sidekar is broken, confusing, flaky, or missing a needed feature. Do not include URLs, company names, usernames, project names, or file paths in the comment.

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

Prefer targets in this order:

1. refs from `ax-tree -i`, `observe`, or `text`
2. `--text "..."` matches
3. CSS selectors
4. `sidekar eval ...` as an escape hatch
5. coordinates as a last resort

## Source Of Truth

The command surface changes over time. Always prefer:

```bash
sidekar --help
sidekar help <command>
sidekar help ext
```

over memorized syntax.
