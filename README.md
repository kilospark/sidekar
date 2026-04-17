# sidekar: the sidecar for AI agents

Sidekar is a coordination and automation substrate for AI CLI agents like Claude Code, Codex, Cursor, Copilot, and Gemini CLI. It adds browser and page automation, data capture, macOS desktop automation, a local message bus with optional cross-machine relay, local memory and task tracking, repo context, background jobs, encrypted key-value storage with TOTP, and Chrome extension control. The point is simple: equip autonomous agents with shared surfaces to communicate and act through, without taking over their control loop.

Sidekar is not an agent orchestrator, not an agent harness, and not an agent OS. It runs alongside your existing agent; it does not replace it. It can also run directly as `sidekar repl`, a standalone LLM agent with streaming, tool calling, and session persistence.

Works with Claude Code, Codex, Cursor, Copilot, Gemini CLI, OpenCode, and other agents through the bundled skill (`sidekar install`) or the [Vercel skills registry](https://github.com/vercel-labs/skills).

## Install

### Recommended

```bash
curl -fsSL https://sidekar.dev/install | sh
```

Downloads the `sidekar` binary, adds it to your `PATH`, and runs `sidekar install` to place `SKILL.md` into each detected agent's skills directory (Claude Code, Codex, Gemini CLI, OpenCode, Pi, etc.).

### Uninstall

```bash
curl -fsSL https://sidekar.dev/uninstall | sh
```

### Agent Skill (registry)

```bash
npx skills add kilospark/sidekar
```

Works with Claude Code, Cursor, Codex, Windsurf, Cline, Copilot, OpenCode, Goose, and [40+ agents](https://github.com/vercel-labs/skills). Powered by Vercel's [skills](https://github.com/vercel-labs/skills) CLI.

### Manual skill install

If you already have the binary:

```bash
sidekar install
```

Or copy `SKILL.md` from this repo into your agent's skills folder (see output of `sidekar install` when no agents are detected).

## Step-by-step

1. **Install sidekar.** Download the binary and install the agent skill in one step:
   ```bash
   curl -fsSL https://sidekar.dev/install | sh
   ```
   This adds `sidekar` to your `PATH` and runs `sidekar install` so `SKILL.md` is copied into each detected agent's skills directory.

2. **Chrome extension (optional).** To drive your everyday Chrome profile (same cookies and logins as the window you already use), load the MV3 extension from the `extension/` directory and click **Login with GitHub** in the popup. The bridge starts automatically. See [`extension/README.md`](extension/README.md) for details.

3. **`sidekar device login` (optional).** Run `sidekar device login` to authenticate this machine with sidekar.dev. This unlocks the relay tunnel, the web terminal, account session management, and account-backed encryption state.

4. **Launch an agent with sidekar.** Run `sidekar <agent> [args...]` where `<agent>` is any CLI on your `PATH` (e.g. `sidekar claude`, `sidekar codex`). Sidekar wraps the process in a PTY, registers it on the bus, opens a tunnel to the relay, and wires up browser and messaging for that session.

   Or run `sidekar repl -c <credential> -m <model>` to use Sidekar's standalone REPL agent mode directly.

## Usage

Just tell your agent what you want:

```
check the top stories on Hacker News
navigate to github.com and show my notifications
search google for "best restaurants near me"
build this component, then open the dev server and verify it renders correctly
```

Or describe any goal; the agent will figure out the steps.

## What it does

The repo context frames Sidekar around four product pillars. The rest of the CLI surface builds on top of them.

### 1. Browser automation

Drive Chrome directly over CDP: launch or connect, navigate, manage tabs and frames, read pages, inspect DOM and accessibility trees, find elements, resolve links, capture screenshots and PDFs, search the web, and read multiple URLs in parallel. The optional Chrome extension exposes a second control surface for your everyday profile, including browser history, active context, and DOM watchers.

### 2. Desktop automation

Control native macOS applications via the Accessibility API: inspect apps and windows, click UI elements, press keys, type and paste text, launch and quit apps, and capture desktop screenshots. This covers the parts of real workflows that live outside CDP-driven browser pages.

### 3. Inter-agent communication and orchestration

Agents coordinate through a local SQLite-backed bus with optional relay support across machines. Sidekar also keeps useful context local: durable memory, task dependencies, local agent session history, repo packing and change summaries, discovered project actions, and output compaction for noisy command results.

### 4. Background automation

Long-running work stays in the binary. `monitor` watches tabs for changes. `cron` schedules tool runs, prompts, or shell commands. `loop` creates recurring prompt jobs tied back to the owning agent session. Remote relay and the web terminal layer sit on top of this so you can check in on a running PTY from another browser when needed.

### Product framing vs. CLI grouping

At the product level, the main buckets are still the four pillars above. The CLI is more granular for discoverability.

| Product bucket | CLI groups |
|------|-------------|
| `Browser` | `Browser`, `Page`, `Interact`, `Data` |
| `Desktop` | `Desktop` |
| `Agent` | `Agent` |
| `Background` | `monitor` under `Agent`, plus `Jobs` (`cron`, `loop`) |
| Supporting layer | `Account` and `System`, plus the extension surface under `ext` and the standalone `repl` mode |

The browser bucket is intentionally broad: the help output splits launch/navigation, page reading, interaction, and page-state inspection into separate headings, but those are one browser automation surface, not four different product pillars. `repl` is different: it is not a pillar itself, but a top-level way to access Sidekar as a standalone agent.

## How it works

The agent follows a **perceive-act loop**:

1. **Plan:** break the goal into steps
2. **Act:** navigate, click, type via CDP commands
3. **Perceive:** read the page to see what happened
4. **Decide:** adapt, continue, or report results
5. **Repeat:** until the goal is done

## Reading the page

sidekar provides multiple ways to read page content, each optimized for different needs:

| Need | Tool | Output |
|------|------|--------|
| Page content (articles, docs) | `read` | Clean text, no UI chrome |
| Full page + interaction targets | `text` | Text + numbered refs |
| Interactive elements only | `ax-tree -i` | Flat list of clickable/typeable elements |
| Interactive elements as commands | `observe` | Ready-to-use action commands |
| HTML structure/selectors | `dom` | Compact HTML |
| Visual layout | `screenshot` | PNG/JPEG image |

**`read`** strips navigation, sidebars, ads, and returns just the main content as clean text with headings, lists, and paragraphs. Best for articles, docs, search results, and information retrieval. Waits for network idle before extraction; retries and falls back to innerText for JS-heavy pages.

**`text`** shows the full page in reading order, interleaving static text with interactive elements (numbered refs). Like a screen reader view. Generates a ref map so you can immediately use `click 12` or `type 3 hello`.

## Sessions

Each agent invocation gets its own **session** with isolated tab tracking. On `launch`, a unique session ID is generated and a fresh Chrome tab is created for that session.

- Multiple agents can work side by side in the same Chrome instance
- Each session only sees and controls its own tabs
- You can also use separate Chrome windows or profiles for stronger isolation when needed

## Profiles

Use profiles to launch isolated browser instances with separate data directories:

```bash
sidekar launch                           # Default shared profile
sidekar launch --profile shopping-bot    # Named profile with its own browser
sidekar launch --profile new             # Auto-generated profile ID
sidekar launch --browser brave --profile test  # Specific browser per profile
sidekar launch --headless                # Headless mode (no visible window)
```

Each profile runs its own browser process. The default profile is persistent and shared. Custom profiles can be killed with `kill`, which closes the browser and cleans up the profile directory.

## Selected commands

### PTY wrapper and agent coordination

```bash
sidekar claude [args]                     # Launch Claude Code in a sidekar PTY
sidekar codex [args]                      # Launch Codex in a sidekar PTY
sidekar repl -c claude -m claude-sonnet-4-5-20250514
sidekar bus who                           # List agents on your channel
sidekar bus send claude-2 "Review the PR" # Send a request or FYI
sidekar memory context                    # Show scoped startup memory brief
sidekar tasks list --ready                # Show unblocked tasks
sidekar repo pack                         # Pack repo files for model context
sidekar monitor start all                 # Watch tabs for title/favicon changes
```

Each wrapped agent gets bus registration, a persistent nickname, and optional relay/web-terminal integration.

### Browser, page, and interaction

```bash
sidekar launch                            # Start Chrome and create a session
sidekar connect                           # Attach to an already-running Chrome
sidekar navigate https://example.com      # Navigate to a URL
sidekar read                              # Reader-mode extraction
sidekar text                              # Full page text with interactive refs
sidekar observe                           # Ready-to-use interaction commands
sidekar search "best ramen near me"       # Search in-browser and extract results
sidekar read-urls https://a.com https://b.com
sidekar screenshot --full                 # Full-page screenshot
sidekar click 12                          # Click by ref after text/observe/ax-tree
sidekar type "#email" "me@example.com"    # Type into a field
sidekar fill "#email" "me@example.com" "#password" "secret"
sidekar wait-for "button[type=submit]"    # Wait for an element
sidekar eval "document.title"             # Run JavaScript in page context
```

### Data capture and page state

```bash
sidekar console                           # Show recent console output
sidekar network capture 10                # Capture XHR/fetch requests
sidekar cookies                           # List cookies for the current page
sidekar storage get                       # Show localStorage
sidekar block images fonts                # Block selected resource types
sidekar viewport desktop                  # Set a viewport preset
sidekar download list                     # Show tracked downloads
sidekar service-workers list              # List service workers
sidekar pack data.json                    # Compact JSON, YAML, or CSV for agent use
sidekar unpack packed.txt                 # Restore packed structured data
```

### Desktop automation and extension bridge

```bash
sidekar desktop apps
sidekar desktop screenshot --app Safari
sidekar desktop click --app Finder "New Folder"
sidekar desktop press Meta+Space
sidekar ext tabs
sidekar ext context
sidekar ext history "terraform vpc"
sidekar ext watch "span.notification-count"
```

### Jobs, account, and system

```bash
sidekar cron list
sidekar cron create "*/5 * * * *" --prompt="check deployment status"
sidekar loop 10m "summarize recent errors" --once
sidekar device login
sidekar session list
sidekar kv set github_token abc123
sidekar totp list
sidekar daemon status
sidekar event list --level=error 100
sidekar install
sidekar update
```

**Ref-based targeting:** after `text`, `observe`, or `ax-tree`, use ref numbers directly as selectors (`click 1`, `type 3 hello`).

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│  sidekar binary (Rust)                                       │
│                                                              │
│  ┌───────────┐  ┌──────────┐  ┌──────────────────┐          │
│  │ Skill      │  │   CLI    │  │   PTY Wrapper    │          │
│  │ install    │  │ dispatch │  │ (fork+exec)      │          │
│  │ (skill.rs) │  │          │  │                  │          │
│  └─────┬──────┘  └────┬─────┘  └────┬─────────────┘          │
│        │              │              │                        │
│  ┌─────▼──────────────▼──────────────▼──────────────────────┐ │
│  │              Command Dispatch                             │ │
│  │  browser/page · interact/data · desktop                   │ │
│  │  bus · memory · tasks · repo · monitor · cron · loop      │ │
│  │  device/session · kv/totp · daemon/config/event · ext     │ │
│  └─────┬──────────────┬──────────────┬──────────────────────┘ │
│        │              │              │                        │
│  ┌─────▼──────┐ ┌─────▼──────┐ ┌────▼────────────┐          │
│  │  CDP Client │ │  Agent Bus │ │ Desktop (macOS) │          │
│  │  WebSocket  │ │  SQLite    │ │ Accessibility   │          │
│  │  to Chrome  │ │  Broker    │ │ API + Screen    │          │
│  └─────────────┘ └─────┬──────┘ └─────────────────┘          │
│                        │                                     │
│              ┌─────────▼─────────┐                           │
│              │  Transport Layer  │                           │
│              │                   │                           │
│              │  Broker (local)   │                           │
│              │  RelayHttp (remote)│                          │
│              └─────────┬─────────┘                           │
│                        │                                     │
│  ┌──────────┐  ┌───────▼───────┐  ┌───────────────────┐     │
│  │ Extension│  │ WSS Tunnel    │  │  Telemetry/       │     │
│  │ bridge   │  │ (tunnel.rs)   │  │  Auto-update      │     │
│  │ (ext.rs) │  │               │  │                   │     │
│  └──────────┘  └───────┬───────┘  └───────────────────┘     │
└────────────────────────┼────────────────────────────────────┘
                         │ WSS
                         ▼
              ┌──────────────────┐
              │ relay.sidekar.dev│
              │                  │
              │  PTY multiplex   │
              │  Bus relay       │
              │  Session registry│
              └──────────────────┘
```

- **Skill installer** (`skill.rs`): Copies `SKILL.md` into detected agent skill directories; `sidekar install` entry point
- **CLI** (`main.rs` -> `commands/mod.rs`): Command dispatch for browser/page/data, desktop, bus, memory/tasks/repo, monitor, cron/loop, account, and system surfaces
- **PTY Wrapper** (`pty.rs`): Fork+exec agents in a PTY, register on bus, open WSS tunnel, bridge I/O, signal forwarding
- **Agent Bus** (`bus.rs` + `broker.rs` + `message.rs`): SQLite-backed agent registry, typed envelope protocol (request/response/fyi/handoff), delivery via SQLite message queue, nudge timers, timeout tracking
- **Transport** (`transport.rs`): `Broker` for local delivery (SQLite queue), `RelayHttp` for cross-machine delivery (HTTPS POST to relay, fanned out to recipient's WSS tunnel)
- **Tunnel** (`tunnel.rs`): Persistent WSS connection to `relay.sidekar.dev`, multiplexes PTY I/O (binary frames) and bus messages (JSON text frames with `ch: "bus"`), heartbeat keepalive, auto-reconnect with exponential backoff
- **CDP Client** (`lib.rs`): Raw WebSocket to Chrome's debug port, request/response matching, event queue, auto-dialog handling, connection retry, TCP keepalive
- **Desktop** (`desktop/`): macOS-only Accessibility API (`objc2-app-kit`), screen capture (`screencapturekit`), input simulation (`enigo`)
- **Monitor** (`commands/monitor.rs`): Background task watches tab titles/favicons via CDP, delivers notifications via bus transport

## vs. Playwright-based tools

Several tools give AI agents browser control on top of Playwright: [agent-browser](https://github.com/vercel-labs/agent-browser) (Vercel), [Playwright MCP](https://github.com/microsoft/playwright-mcp) (Microsoft), [Stagehand](https://github.com/browserbase/stagehand) (Browserbase), and [Browser Use](https://github.com/browser-use/browser-use).

|  | **sidekar** | **Playwright-based tools** |
|--|-----------|--------------------------|
| **What it is** | Rust binary: CDP + extension bridge, desktop, agent bus, local memory/tasks, background jobs | SDK / CLI wrapping Playwright (often via MCP) |
| **Architecture** | Direct CDP WebSocket to your Chrome | CLI/SDK -> IPC -> Playwright -> bundled Chromium |
| **Install size** | Single binary, zero deps | ~200 MB+ (node_modules + Chromium download) |
| **Uses your browser** | Yes - your Chrome, your cookies, your logins | No - launches bundled Chromium with clean state |
| **User agent** | Your real Chrome user agent | Modified Playwright/Chromium UA - detectable |
| **Headed mode** | Always (unless `--headless`) - you see what the agent sees | Headless by default |
| **Multi-agent** | Built-in bus for agent discovery and messaging (local + cross-machine) | None |
| **Desktop control** | macOS Accessibility API integration | None |

## Configuration

Stored in `~/.config/sidekar/sidekar.json`:

| Key | Default | Description |
|-----|---------|-------------|
| `browser` | auto-detect | Preferred browser (chrome, edge, brave, arc, vivaldi, chromium) |
| `auto_update` | `true` | Auto-check and download updates hourly |
| `max_tabs` | `20` | Maximum tabs per session |
| `cdp_timeout_secs` | `60` | CDP command timeout |

## Build from source

```bash
git clone https://github.com/kilospark/sidekar.git
cd sidekar
cargo build --release
# Binary: target/release/sidekar (CLI; run `sidekar install` for agent skills)
```

## Requirements

- Any Chromium-based browser: Google Chrome, Microsoft Edge, Brave, Arc, Vivaldi, Opera, or Chromium
- No runtime dependencies (single Rust binary)
- macOS for desktop automation features (Accessibility + Screen Recording permissions required)

Auto-detected on macOS, Linux, Windows, and WSL. Set `CHROME_PATH` to override.

## Project structure

```
src/
├── main.rs              # CLI entry point, arg parsing, session discovery
├── lib.rs               # AppContext, CdpClient, CDP helpers, session state
├── skill.rs             # SKILL.md install paths for agent CLIs
├── ext.rs               # Chrome extension bridge and native host
├── commands/
│   ├── mod.rs           # Command dispatch table (~80 commands)
│   ├── core.rs          # launch, connect, navigate, read, text, dom, ax-tree,
│   │                    #   screenshot, click, type, press, tabs, search, read-urls
│   ├── data.rs          # cookies, console, network, block, viewport, zoom,
│   │                    #   frames, media, animations, security, storage, service-workers
│   ├── session.rs       # download, activate, minimize, lock/unlock, human-click/type
│   ├── desktop.rs       # macOS desktop automation commands
│   ├── batch.rs         # Multi-action batch execution
│   ├── cron.rs          # Scheduled jobs
│   ├── monitor.rs       # Tab watching background task
│   └── interaction/     # Click dispatch, forms, waiting, query helpers
├── bus.rs               # Agent bus: registration, messaging, nudge timers
├── broker.rs            # SQLite-backed agent registry and message persistence
├── message.rs           # Typed envelope protocol (AgentId, Envelope, MessageKind)
├── transport.rs         # Transport trait, Broker (local SQLite), RelayHttp (remote HTTPS)
├── poller.rs            # Message queue poller, delivers queued messages to agents
├── ipc.rs               # Legacy stub (sockets replaced by SQLite queue)
├── pty.rs               # PTY wrapper for launching agents (fork+exec, tunnel, bus)
├── tunnel.rs            # WSS tunnel client to relay.sidekar.dev (PTY + bus multiplex)
├── desktop/
│   ├── macos.rs         # Accessibility API: find, click, list apps/windows
│   ├── screen.rs        # Screen capture via ScreenCaptureKit
│   ├── input.rs         # Input simulation via enigo
│   └── types.rs         # AppInfo, WindowInfo, UIElement types
├── scripts.rs           # Embedded JS scripts (page brief, DOM extract, ax-tree, etc.)
├── types.rs             # SessionState, DebugTab, InteractiveElement, etc.
├── utils.rs             # Browser detection, key mapping, file helpers
├── config.rs            # SidekarConfig (JSON in ~/.config/sidekar/)
├── api_client.rs        # Version check, self-update
└── auth.rs              # Device auth flow (GitHub OAuth)

relay/                   # Relay server (Fly.io deployed, WSS + bus fan-out)
www/                     # Landing page + API (Vercel)
extension/               # Chrome extension (MV3)
```

## Contributing

Issues and PRs welcome at [github.com/kilospark/sidekar](https://github.com/kilospark/sidekar). Build from source with `cargo build --release`; tests run via `cargo test`.

## License

MIT — see [LICENSE](LICENSE). Portions of this project were ported from or inspired by other open-source projects; see [CREDITS.md](CREDITS.md) for attribution.
