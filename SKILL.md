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

## Delegating to another agent

Launch a second agent, wait for it on the bus, then read what it found:

```bash
REVIEWER=$(sidekar spawn codex "Review the diff on this branch. Reply with findings.")
sidekar bus wait "$REVIEWER"
sidekar bus replies --limit=5
sidekar stop "$REVIEWER"
```

`spawn` prints the new agent's bus name and nothing else, so it composes. It
picks the unattended-mode flag for that particular CLI — every one of them
spells it differently — and runs the agent detached so it survives your turn.

Headless by default. Add `--window` to open it in a real terminal window the
human can watch and type into; it reuses whichever terminal app you are running
under, and `--app` overrides that. Add `--log <path>` for a transcript either
way.

Delegate work that is genuinely separable: an independent review, a second
opinion, a long build, a task in another repo via `--cwd`. A spawned agent is
yours to finish — read its reply and stop it. One you never read is spent
tokens, and one you never stop is a process nobody owns.

## Operating Rules

1. Use CLI help for exact syntax — never invent flags or subcommands.
2. Check `sidekar bus who` before assuming you are working alone; it flags agents that
   finished a turn nobody has looked at.
3. To depend on another agent, `sidekar bus wait <agent>` instead of polling `bus who`.
   If a message will not land or a wait keeps timing out, run `sidekar bus explain <agent>`.
4. To delegate, `sidekar spawn <agent> "<task>"` and address the name it prints. Never
   assemble another CLI's permission flags yourself — spawn knows each one.
5. Stop what you spawn: `sidekar spawn list`, then `sidekar stop <name>` when done.
6. Use `sidekar kv` for any secret or credential — never store in plain files.
7. Use `sidekar totp get` during login flows that require 2FA codes.
8. Write durable learnings to `sidekar memory write` so future sessions benefit.
9. Pipe noisy command output through `sidekar compact filter` or use `sidekar compact run`.
10. After state-changing browser actions, read the returned brief before deciding next step.
11. Prefer `read`, `ax-tree -i`, or `text` before taking screenshots.
12. Prefer refs from `ax-tree -i` or `observe` over CSS selectors; coordinates only as last resort.
13. If login, CAPTCHA, or 2FA blocks browser progress, run `sidekar activate` and tell the user.
14. Never touch browser tabs you did not create. Close tabs you opened when done.
