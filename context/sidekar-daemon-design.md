# Sidekar Daemon Design

## Problem

sidekar has multiple background process concepts without a single owning daemon:

- **ext bridge**: persistent Chrome extension bridge
- **Monitor** (planned): watch browser tabs for state changes, notify agents
- **Cron** (planned): run scheduled actions on intervals, notify agents
- **Bus poller**: background message delivery to agent PTYs

These should be one process, not four.

## Decision

Introduce `sidekar daemon` — a single background process that owns all long-running subsystems.

All CLI tools and the native messaging host talk to the daemon over a unix socket. The daemon runs, persists state, and delivers results.

## Architecture

```
sidekar daemon
├── ext-bridge subsystem
│   └── registered native-messaging bridge → Chrome extension connection
├── monitor subsystem (reactive)
│   └── watch tabs via CDP → detect changes → bus send to requesting agent
├── cron subsystem (proactive)
│   └── schedule fires → execute tool/batch → bus send result to target agent
├── bus-housekeeping subsystem
│   └── cleanup old messages, orphaned agents
└── unix socket (control interface)
    ├── ext commands (tabs, click, read, etc.)
    ├── ext bridge registration from native host
    ├── monitor_start / monitor_stop
    ├── cron_create / cron_list / cron_delete
    └── status / stop
```

## What gets consolidated

| Before | After |
|--------|-------|
| `sidekar ext-server` (separate process) | ext-bridge subsystem in daemon |
| IPC TCP port (9877) for CLI→ext-server | Unix socket in daemon |
| Extension bootstrap over localhost WS port | Persistent native messaging bridge |
| Native host bootstraps ext-server | Native host registers bridge with daemon |
| Planned cron daemon | cron subsystem in daemon |
| Planned monitor daemon | monitor subsystem in daemon |

## Lifecycle

- **Auto-launch**: first command that needs daemon (ext, monitor, cron) starts it if not running
- **Persist**: daemon stays running across CLI/MCP sessions
- **Native messaging**: Chrome extension triggers daemon via native host
- **Graceful shutdown**: `sidekar daemon stop` or SIGTERM
- **PID file**: `~/.sidekar/daemon.pid`
- **Socket**: `~/.sidekar/daemon.sock` (permissions 0600)
- **Logs**: stderr or `~/.sidekar/daemon.log`

### Migration from ext-server

1. `sidekar ext-server` becomes `sidekar daemon` (or auto-launches daemon)
2. `sidekar ext stop` becomes `sidekar daemon stop`
3. Native host registers a persistent extension bridge with the daemon
4. IPC TCP port (9877) removed — use unix socket
5. PID file moves from `~/.sidekar/ext-server.pid` to `~/.sidekar/daemon.pid`

## Cron Subsystem

### Relationship to Claude Code's built-in cron

Claude Code has CronCreate/CronList/CronDelete — session-only, in-memory, gone when Claude exits. sidekar cron is complementary:

| | Claude Code Cron | sidekar Cron |
|---|---|---|
| Lifetime | Session-only | Persistent across sessions |
| Fires when | REPL is idle | Daemon is running (always) |
| Action | Enqueues a prompt to Claude | Executes sidekar tool/batch, delivers via bus |
| Storage | In-memory | Persisted to disk |
| Use case | "Remind me in 30 min" | "Check this dashboard every 5 min and alert me if anything changes" |

### Tool shape

```
cron_create:
  schedule: "*/5 * * * *"      # standard 5-field cron, local timezone
  action:                       # what to do when it fires
    tool: "screenshot"          # sidekar tool name
    args: { "url": "..." }     # tool arguments
    # OR
    batch: [...]               # batch sequence
  target: "agent-name"         # who gets the result via bus send
  name: "dashboard-check"     # human-readable label (optional)

cron_list:
  # returns all active cron jobs with next fire time

cron_delete:
  id: "..."                    # job ID from cron_create
```

### Persistence

Cron jobs stored in SQLite (`cron_jobs` table). Survives daemon restarts.

### Delivery

When a cron job fires:
1. Daemon executes the action (tool call or batch)
2. Result is delivered via `bus send` to the target agent
3. If no agent is listening — **table this for now** (options: queue, drop, log)

## Monitor Subsystem

Existing design from `project_monitor_design.md` moves into the daemon:

- Watches browser tabs via CDP for title/favicon changes
- Detects state changes (new Slack message, new email, build status change)
- Delivers notifications via `bus send` to the requesting agent

The MCP `monitor` tool becomes a thin client:
- `monitor` with `action: "start"` → sends `monitor_start` to daemon socket
- `monitor` with `action: "stop"` → sends `monitor_stop` to daemon socket

## Why One Daemon

1. **Shared socket**: one well-known address for all background services
2. **Shared lifecycle**: one process to manage, one PID file, one log stream
3. **Shared bus access**: daemon has one bus connection, routes to agents
4. **Simpler browser bridge**: extension talks through native messaging, daemon stays the source of truth
5. **No IPC TCP port**: unix socket is cleaner, more secure (file permissions)
6. **Extensible**: future background capabilities (file watchers, webhook listeners) slot in as subsystems

## Relationship to Orchestration Plan

The daemon is **not** the broker from the orchestration plan. The broker is a heavier concept (session registry, task/lease model, capability routing). The daemon is lighter — it just runs background work and delivers results via the bus.

If/when the broker is built, the daemon's subsystems could migrate into it. For now, the daemon is a practical stepping stone that delivers value immediately.

## Implementation Order

1. **Daemon skeleton**: process management, unix socket, PID file
2. **Absorb ext-server**: move extension bridge state into daemon
3. **Update native host**: register a persistent bridge with the daemon socket
4. **Remove IPC TCP port**: CLI ext commands go through unix socket
5. **Monitor subsystem**: tab watching via CDP, bus notifications
6. **Cron subsystem**: schedule persistence, execution, bus delivery
7. **Deprecate ext-server**: `sidekar ext-server` becomes alias for `sidekar daemon`
