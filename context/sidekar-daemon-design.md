# Sidekar Daemon Design

## Problem

sidekar needs background capabilities that outlive individual MCP sessions:

- **Monitor** (reactive): watch browser tabs for state changes, notify agents
- **Cron** (proactive): run scheduled actions on intervals, notify agents

Bolting cron onto the `monitor` tool creates naming confusion — monitoring implies passive observation, cron is active scheduling. Both need a long-running process, so they should share one.

## Decision

Introduce `sidekar daemon` — a single background process that owns all long-running subsystems.

MCP tools (`monitor`, `cron_create`, etc.) become thin clients that talk to the daemon over a unix socket. The daemon is the thing that actually runs, persists state, and delivers results via bus messages.

## Architecture

```
sidekar daemon
├── monitor subsystem (reactive)
│   └── watch tabs via CDP → detect changes → bus_send to requesting agent
├── cron subsystem (proactive)
│   └── schedule fires → execute tool/batch → bus_send result to target agent
└── unix socket (receives commands from MCP sessions)
    ├── monitor_start { session, tabs, patterns }
    ├── monitor_stop { session }
    ├── cron_create { schedule, action, target }
    ├── cron_list
    ├── cron_delete { id }
    └── status
```

## Lifecycle

- **Auto-launch**: first MCP session that needs monitor or cron starts the daemon if not running
- **Persist across sessions**: MCP sessions come and go, daemon stays
- **Graceful shutdown**: daemon exits when no active monitors or cron jobs remain (or after idle timeout)
- **PID file**: `~/.sidekar/daemon.pid` for lifecycle management
- **Socket**: `~/.sidekar/daemon.sock` (permissions 0600)

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
  target: "agent-name"         # who gets the result via bus_send
  name: "dashboard-check"     # human-readable label (optional)

cron_list:
  # returns all active cron jobs with next fire time

cron_delete:
  id: "..."                    # job ID from cron_create
```

### Persistence

Cron jobs stored in `~/.config/sidekar/cron.json` (or SQLite if complexity warrants it). Survives daemon restarts.

### Delivery

When a cron job fires:
1. Daemon executes the action (tool call or batch)
2. Result is delivered via `bus_send` to the target agent
3. If no agent is listening — **table this for now** (options: queue, drop, log)

## Monitor Subsystem

Existing design from `project_monitor_design.md` moves into the daemon:

- Watches browser tabs via CDP for title/favicon changes
- Detects state changes (new Slack message, new email, build status change)
- Delivers notifications via `bus_send` to the requesting agent

The MCP `monitor` tool becomes a thin client:
- `monitor` with `action: "start"` → sends `monitor_start` to daemon socket
- `monitor` with `action: "stop"` → sends `monitor_stop` to daemon socket

## Why One Daemon

1. **Shared socket**: one well-known address for all background services
2. **Shared lifecycle**: one process to manage, one PID file, one log stream
3. **Shared bus access**: daemon has one bus connection, routes to agents
4. **Extensible**: future background capabilities (file watchers, webhook listeners) slot in as subsystems without new processes

## Relationship to Orchestration Plan

The daemon is **not** the broker from the orchestration plan. The broker is a heavier concept (session registry, task/lease model, capability routing). The daemon is lighter — it just runs background work and delivers results via the bus.

If/when the broker is built, the daemon's subsystems could migrate into it. For now, the daemon is a practical stepping stone that delivers value immediately.

## Implementation Order

1. Daemon skeleton: process management, socket, PID file
2. Monitor subsystem: move existing monitor design into daemon
3. Cron subsystem: schedule persistence, execution, delivery
4. MCP tool updates: monitor and cron_* tools become socket clients
