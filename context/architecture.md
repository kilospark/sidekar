# Sidekar Architecture

Single Rust binary (`sidekar`) that provides browser automation, desktop
control, inter-agent messaging, background jobs, and local state management
for AI coding agents.

## Process Model

```
                              +-------------------+
                              |   sidekar daemon   |
                              |   (background)     |
                              |                    |
                              | - CDP pool         |
                              | - ext bridge       |
                              | - housekeeping     |
                              | - idle reaper      |
                              +--------+-----------+
                                       |
                           unix socket (daemon.sock)
                                       |
       +---------------+---------------+---------------+
       |               |               |               |
  sidekar cli     sidekar cli     PTY wrapper     PTY wrapper
  (one-shot)      (one-shot)     (long-lived)    (long-lived)
                                      |               |
                                 agent child      agent child
                                 (claude)          (codex)
```

### Daemon (`src/daemon.rs`)

Single background process, started lazily by `ensure_running()`. Owns:

- **CDP connection pool** (`src/cdp_proxy.rs`): persistent WebSocket connections
  to Chrome, keyed by `ws_url`. WS ping every 15s, 120s idle timeout. CLI
  commands proxy CDP requests through the daemon via unix socket instead of
  creating per-call WS connections.
- **Extension bridge** (`src/ext.rs`): native messaging host for the Chrome
  extension. Bidirectional JSON stream over unix socket.
- **Housekeeping**: dead agent sweeper (60s), stale message cleanup (1h),
  auto-update check (1h).
- **CDP pool reaper**: closes idle connections every 30s.

IPC protocol: JSON-line over unix socket (`~/.sidekar/daemon.sock`).
Line reads capped at 1MB to prevent memory pressure from malicious clients.

Stale `daemon.pid`/`daemon.sock` are cleaned on `ensure_running()` after
kill -9 or crash.

### PTY Wrapper (`src/pty.rs`)

`sidekar <agent>` wraps the agent process in a sidekar-owned PTY. This gives:

- **Bus registration**: agent identity on the message bus
- **Message injection**: poller writes broker messages into the PTY master fd
- **Cron loop**: background tokio task with supervisor (auto-restart on panic)
- **Relay tunnel**: WebSocket tunnel to `relay.sidekar.dev` for web terminal
- **Session watcher**: detects when the child launches a browser

Each PTY wrapper is independent. Multiple agents can run concurrently.

### CLI Commands (`src/main.rs`, `src/commands/`)

Each `sidekar <command>` invocation is a separate process. Flow:

1. Check if command is ext-routable AND extension is available AND **not** in PTY
   - If yes: route through extension (user's normal Chrome)
   - If no: continue to CDP path
2. Discover session: read per-agent `last-session-{name}` file (never falls back
   to generic `last-session` when `SIDEKAR_AGENT_NAME` is set)
3. If no session and `auto_launch_browser`: launch Chrome, create session
4. `open_cdp()`: try daemon proxy first, fall back to direct WS if unavailable
5. Dispatch command

## Browser Automation

### CDP Connection (`src/lib.rs`, `src/cdp_proxy.rs`)

```
CLI process                    Daemon                         Chrome
  |                              |                              |
  |-- cdp_connect (ws_url) ----->|                              |
  |                              |-- WS connect (if new) ------>|
  |-- cdp_send {method,params} ->|                              |
  |                              |-- WS {id, method, params} -->|
  |                              |<-- WS {id, result} ----------|
  |<-- cdp_resp {req_id,result} -|                              |
  |                              |                              |
  |<-- cdp_event {method,params} | (subscribed events)          |
```

**CdpClient** (`src/lib.rs`): enum with two variants:
- `Direct(DirectCdp)`: ephemeral per-call WS connection (fallback)
- `Proxied(DaemonCdpProxy)`: routes through daemon's persistent connection

All command code uses `CdpClient` — the variant is transparent.

**Connection pool** (`src/cdp_proxy.rs`): one `connection_task` per `ws_url`,
manages WS keepalive (ping every 15s), routes responses by CDP message ID,
broadcasts events to all subscribers. Dead connections are removed from the
pool HashMap and recreated on next request.

**Timeouts**:
- WS handshake: 10s
- CDP method call: configurable (`cdp_timeout_secs`, default 60s)
- HTTP body read: 10s
- IPC oneshot response: 120s
- Chrome launch readiness: 30s

### Session Isolation

Each agent gets its own Chrome session:

- **Session pointer**: `~/.sidekar/last-session-{sanitized_agent_name}`
- **Session state**: `~/.sidekar/state-{session_id}.json` (flock-protected)
- **Tab ownership**: `state.tabs` lists tab IDs owned by this session
- **Port isolation**: each Chrome instance uses a different debug port

Guards:
- PTY agents skip extension routing (use CDP path exclusively)
- No fallback to generic `last-session` file for named agents
- `connect_to_tab` checks `other_sessions_on_port` before creating tabs
- Stale tab IDs pruned from session state on each connect
- Tab lock mechanism prevents concurrent access to the same tab

### Extension Automation (`src/ext.rs`)

`sidekar ext <command>` automates the user's normal Chrome profile via the
sidekar Chrome extension. Extension connects to daemon via native messaging.

Extension routing is **disabled** inside PTY wrappers to maintain session
isolation. Only bare CLI invocations (outside PTY) use the extension.

## Inter-Agent Communication

### Bus (`src/bus.rs`, `src/broker.rs`, `src/poller.rs`)

SQLite-backed message bus (`~/.sidekar/sidekar.sqlite3`).

- **Agents register** via PTY wrapper with name, label, channel, pane
- **Messages**: `bus send <to> <message>`, `bus done <next> <summary> <request>`
- **Broker tables**: `agents`, `bus_queue`, `pending_requests`, `outbound_requests`,
  `bus_replies`, `agent_sessions`
- **Poller** (`src/poller.rs`): background thread in each PTY wrapper, polls
  broker every 500ms, injects messages into agent's PTY via master fd
- **Nudges**: automatic follow-up reminders for unanswered requests (exponential
  backoff: 60s, 120s, 300s, 600s, 900s, max 5)
- **Transport**: broker (local SQLite) or relay_http (remote via relay)

### Relay (`relay/`)

WebSocket relay at `relay.sidekar.dev` (GCP, 2 VMs). Bridges:
- PTY tunnel connections (web terminal)
- Remote bus messages between devices

Owner-aware routing: each live session has one owning relay instance in MongoDB.
Browser connects directly to the owning relay's public origin.

## Background Automation

### Cron (`src/commands/cron.rs`)

Runs as tokio task inside the PTY wrapper. Ticks every 60s, aligned to
minute boundaries. Supervisor auto-restarts the loop on panic.

**Action types**:
- `Tool { tool, args }`: dispatch sidekar command
- `Batch { batch }`: sequence of tools
- `Bash { command }`: `sh -c` subprocess
- `Prompt { prompt }`: inject text into agent's PTY (like typing)

**`loop` command**: shortcut for prompt-action cron with human intervals
(`5m`, `1h`, `120s`). Supports `--once` for one-shot execution.

**Anti-replication**: `SIDEKAR_CRON_DEPTH` env var (best-effort, blocks common
case), `max_cron_jobs` (default 10, hard cap).

**Delivery**: output goes via `broker::enqueue_message` to the target agent.
Prompt actions deliver raw text (no prefix). Other actions prepend
`[from sidekar-cron]`.

### Monitor (`src/commands/monitor.rs`)

Watches Chrome tabs for title/favicon changes via CDP
`Target.setDiscoverTargets`. Delivers notifications via broker. Uses direct
`DirectCdp` connection (not proxied) since it's long-lived.

## Local State

### Config (`src/config.rs`)

SQLite config table in `~/.sidekar/sidekar.sqlite3`. Keys: `telemetry`,
`feedback`, `browser`, `auto_update`, `relay_pty`, `max_tabs`,
`cdp_timeout_secs`, `max_cron_jobs`.

### KV Store (`src/commands/kv.rs`)

Encrypted key-value store in broker SQLite. Unencrypted without login;
AES-256-GCM encryption when logged in (key fetched from server).

### Memory (`src/memory.rs`)

Durable project-scoped memory. Stored in broker SQLite with project key
derived from current working directory.

### Tasks (`src/tasks.rs`)

Local task list with dependency edges. Project-scoped, stored in broker.

## Desktop Automation (`src/desktop/`)

macOS-only. Pure Rust via objc2 + core-foundation + ApplicationServices FFI.
No Swift, no external dependencies.

Capabilities: find UI elements, click, type, screenshot, launch/quit apps,
list windows, activate apps.

## Distribution

- **Binary**: single `sidekar` binary, 4 targets (darwin-arm64, darwin-x64,
  linux-x64, linux-arm64)
- **Install**: `curl -fsSL https://sidekar.dev/install | sh`
- **Release**: `./local-release.sh` (build + sign + GitHub release + Vercel + install)
- **Signing**: minisign (binary), codesign (macOS local)
- **Skill discovery**: `sidekar install` copies SKILL.md to agent skill directories

## File Layout

```
~/.sidekar/
  sidekar.sqlite3             # config, bus, cron, kv, memory, tasks
  daemon.pid                  # daemon process ID
  daemon.sock                 # daemon unix socket
  profiles/{name}/            # Chrome user data dirs
    cdp-port                  # Chrome debug port for this profile
  last-session                # generic session pointer (CLI without PTY)
  last-session-{agent}        # per-agent session pointer
  state-{session_id}.json     # session state (tabs, port, active tab)
  state-{session_id}.lock     # flock for concurrent access
```

## Source Layout

```
src/
  main.rs                     # CLI entry, arg parsing, routing
  lib.rs                      # AppContext, DirectCdp, CdpClient enum, helpers
  cdp_proxy.rs                # daemon CDP pool + CLI proxy client
  daemon.rs                   # background daemon process
  pty.rs                      # PTY wrapper for agents
  poller.rs                   # broker message poller + PTY injection
  broker.rs                   # SQLite broker (bus, config, cron, queue)
  bus.rs                      # bus command implementations
  ext.rs                      # Chrome extension bridge
  transport.rs                # message transport (broker, relay_http)
  tunnel.rs                   # WebSocket tunnel to relay
  message.rs                  # message types and envelope format
  config.rs                   # configuration management
  auth.rs                     # device auth + JWT tokens
  api_client.rs               # sidekar.dev API client
  memory.rs                   # durable memory system
  tasks.rs                    # task tracking with dependencies
  types.rs                    # shared types (SessionState, DebugTab, etc.)
  utils.rs                    # human-like input simulation
  scripts.rs                  # injected JS scripts
  cli.rs                      # command specs, help text
  skill.rs                    # SKILL.md installation
  repo.rs                     # repo context and packing
  scope.rs                    # project scope detection
  pakt.rs                     # compact data packing
  rtk.rs                      # repo toolkit
  desktop/                    # macOS desktop automation (objc2 FFI)
  commands/
    mod.rs                    # command dispatch
    core.rs                   # launch, connect, navigate, read, click, etc.
    session.rs                # tab, new-tab, close, frame, activate
    data.rs                   # screenshot, dom, ax-tree, console, network, etc.
    interaction/              # click, type, fill, hover, scroll, drag, etc.
    batch.rs                  # batch command execution
    cron.rs                   # cron subsystem (schedule, execute, loop)
    monitor.rs                # tab change monitoring
    kv.rs                     # encrypted key-value store
    totp.rs                   # TOTP secret management
    agent_sessions.rs         # agent session history
    desktop.rs                # desktop automation commands
relay/                        # WebSocket relay (separate Rust crate)
www/                          # sidekar.dev website (Vercel)
```
