---
name: sidekar
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

Run `sidekar help` to see all commands grouped by category.
Run `sidekar help <command>` for detailed usage, options, and examples on any command.

## Operating Rules

1. Use CLI help for exact syntax — never invent flags or subcommands.
2. Check `sidekar bus who` before assuming you are working alone; it flags agents that
   finished a turn nobody has looked at.
3. To depend on another agent, `sidekar bus wait <agent>` instead of polling `bus who`.
   If a message will not land or a wait keeps timing out, run `sidekar bus explain <agent>`.
4. Use `sidekar kv` for any secret or credential — never store in plain files.
5. Use `sidekar totp get` during login flows that require 2FA codes.
6. Write durable learnings to `sidekar memory write` so future sessions benefit.
7. Pipe noisy command output through `sidekar compact filter` or use `sidekar compact run`.
8. After state-changing browser actions, read the returned brief before deciding next step.
9. Prefer `read`, `ax-tree -i`, or `text` before taking screenshots.
10. Prefer refs from `ax-tree -i` or `observe` over CSS selectors; coordinates only as last resort.
11. If login, CAPTCHA, or 2FA blocks browser progress, run `sidekar activate` and tell the user.
12. Never touch browser tabs you did not create. Close tabs you opened when done.
