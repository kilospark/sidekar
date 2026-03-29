# sidekar: the sidecar for AI agents

Sidekar is a coordination and automation substrate for AI CLI agents like Claude Code, Codex, Cursor, Copilot, and Gemini CLI. It adds a local message bus with optional cross-machine relay, token-efficient browser automation through a dedicated CLI and optional Chrome extension, macOS desktop automation, background monitoring and cron, and encrypted key-value storage with TOTP. The point is simple: equip autonomous agents with shared surfaces to communicate and act through, without taking over their control loop.

Sidekar is not an agent orchestrator, not an agent harness, and not an agent OS. It runs alongside your existing agent; it does not replace it.

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

3. **`sidekar login` (optional).** Run `sidekar login` to authenticate with sidekar.dev. This unlocks the relay tunnel, the web terminal, and the dashboard -- everything that connects agents and sessions across machines.

4. **Launch an agent with sidekar.** Run `sidekar <agent> [args...]` where `<agent>` is any CLI on your `PATH` (e.g. `sidekar claude`, `sidekar codex`). Sidekar wraps the process in a PTY, registers it on the bus, opens a tunnel to the relay, and wires up browser and messaging for that session.

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

Six capability pillars. Everything else (web search, multi-page reads, batch runs) is a use case on top of them.

### 1. Agent communication bus

Agents find each other and coordinate through a shared message bus. On a single machine, messages flow through a SQLite broker. Across machines, a persistent WSS tunnel to `relay.sidekar.dev` carries bus traffic alongside PTY data on the same connection. From the agent's perspective there's no difference -- `bus_send` delivers locally or remotely depending on where the recipient is, and `who` lists everyone reachable.

Messages use a typed envelope protocol with four kinds: **request**, **response**, **fyi**, and **handoff**. Each carries a message ID, timestamp, and threading info. Unanswered requests trigger automatic nudge reminders. Agents get auto-assigned nicknames that persist per project across restarts, and `bus_send @all` broadcasts to every agent on the channel.

### 2. Web terminal

Every PTY session streams live to [sidekar.dev/terminal](https://sidekar.dev/terminal). Open it from your phone, a tablet, or any browser and see exactly what your agents are doing in real time. The terminal renders full xterm output with scrollback, so you get the same view as the local terminal window. Combined with the relay tunnel, this means you can start an agent on your desktop, walk away, and check on it from anywhere.

### 3. Browser automation (CDP + Chrome extension)

Direct Chrome DevTools Protocol over WebSocket -- navigate, click, type, fill forms, handle dialogs, capture network traffic, screenshot, and export PDFs. This is your actual Chrome with your cookies and logins, not a sandboxed Chromium instance. The optional **Chrome extension** bridges the same commands into your everyday browser profile for sites that need in-page context. Page perception is token-efficient: compact summaries instead of raw DOM dumps.

### 4. Desktop automation (macOS)

Control native applications via the macOS Accessibility API: find and click UI elements, desktop screenshots, launch and quit apps, list windows. Runs without Chrome when you need native UI, including permission dialogs and surfaces outside CDP-driven pages.

### 5. Background automation

**Monitor:** watch tab titles and favicons, debounced, with notifications routed through the bus. **Cron:** run tools on a schedule (standard cron expressions), persist jobs in SQLite, deliver results to agents. Together they cover unattended and reactive work.

### 6. Credentials and encrypted storage

This is what closes the loop on fully autonomous agents. The **encrypted KV store** holds usernames, passwords, API keys, and any other secrets -- AES-256-GCM encrypted at rest. **TOTP** stores 2FA secrets and generates time-based codes on demand. Together with browser automation, an agent can log in to any service end-to-end: pull credentials from KV, generate a TOTP code, fill the login form, handle MFA, and proceed -- without secrets ever appearing in chat history or tool output. **Error log** (`sidekar errors`) surfaces recent failures for debugging.

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
| Interactive elements only | `axtree -i` | Flat list of clickable/typeable elements |
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

## CLI

### Agent bus

```bash
sidekar who                     # List agents on your channel (local + relay)
sidekar bus_send <to> <message> # Send a message to another agent (or @all)
sidekar bus_done <next>         # Hand off to another agent with summary
sidekar register [name]         # Register on the bus
sidekar unregister              # Leave the bus
```

### PTY wrapper (recommended for multi-agent)

```bash
sidekar claude [args]           # Launch Claude Code in a sidekar PTY
sidekar codex [args]            # Launch Codex in a sidekar PTY
sidekar copilot [args]          # Launch any agent with bus integration
```

Each agent gets automatic bus registration, a persistent nickname, a relay tunnel, and its PTY output streamed to the [web terminal](https://sidekar.dev/terminal).

### Browser control

```bash
sidekar launch                  # Start browser, create session
sidekar connect                 # Create new session on existing browser
sidekar navigate <url>          # Go to URL (auto-dismisses cookie banners)
sidekar back / forward / reload # Navigation history
sidekar activate                # Bring browser window to front (macOS)
sidekar minimize                # Minimize browser window (macOS)
sidekar kill                    # Kill custom profile session
```

### Page reading

```bash
sidekar read [selector]         # Reader-mode text extraction (strips nav/sidebar/ads)
sidekar text [selector]         # Full page in reading order with interactive refs
sidekar dom [selector]          # Get compact DOM HTML
sidekar dom --tokens=N          # Truncate DOM to ~N tokens
sidekar axtree                  # Full accessibility tree (auto-capped at ~4k tokens)
sidekar axtree -i               # Interactive elements with ref numbers
sidekar axtree -i --diff        # Show only changes since last snapshot
sidekar observe                 # Interactive elements as ready-to-use commands
sidekar find <query>            # Find element by description
sidekar screenshot              # Capture screenshot (default JPEG, 800px wide)
sidekar screenshot --full       # Full page screenshot
sidekar screenshot --ref=N      # Screenshot a specific element by ref
sidekar pdf [path]              # Save page as PDF
```

### Interaction

```bash
sidekar click <sel|x,y|--text>  # Click by selector, coordinates, or text match
sidekar click --mode=double <sel> # Double-click
sidekar click --mode=right <sel>  # Right-click (context menu)
sidekar click --mode=human <sel>  # Click with human-like Bezier mouse movement
sidekar hover <sel>             # Hover (tooltips/menus)
sidekar focus <selector>        # Focus an element without clicking
sidekar clear <selector>        # Clear an input field
sidekar type <selector> <text>  # Type into an input (focuses first, verifies)
sidekar type --human <sel> <text> # Type with variable delays
sidekar keyboard <text>         # Type at current caret position (no selector)
sidekar inserttext <text>       # Insert text via CDP Input.insertText (fast)
sidekar paste <text>            # Paste via clipboard event
sidekar clipboard --html <html> # Paste rich HTML via real clipboard (Google Docs, Notion)
sidekar fill <fields_json>      # Fill multiple form fields at once
sidekar select <sel> <value>    # Select option(s) from a dropdown
sidekar upload <sel> <file>     # Upload file(s) to a file input
sidekar drag <from> <to>        # Drag from one selector to another
sidekar dialog accept|dismiss   # Handle alert/confirm/prompt dialogs
sidekar press <key>             # Press a key or combo (Enter, Ctrl+A, Meta+C)
sidekar scroll <target> [px]    # Scroll: up, down, top, bottom, or selector
```

### Waiting

```bash
sidekar waitfor <sel> [ms]      # Wait for element to appear (default 5s)
sidekar waitfornav [ms]         # Wait for navigation to complete (default 10s)
```

### Web research

```bash
sidekar search <query>          # Search the web (Google by default)
sidekar search --engine=bing <q> # Search via Bing, DuckDuckGo, or custom URL
sidekar readurls <url1> <url2>  # Read multiple URLs in parallel
sidekar resolve <sel>           # Get link/form target URL without clicking
```

### Tabs and frames

```bash
sidekar tabs                    # List this session's tabs
sidekar tab <id>                # Switch to a session-owned tab
sidekar newtab [url]            # Open a new tab in this session
sidekar close                   # Close current tab
sidekar frames                  # List all frames/iframes
sidekar frame <id|sel>          # Switch to a frame
sidekar frame main              # Return to main frame
```

### Developer tools

```bash
sidekar eval <js>               # Run JavaScript in page context
sidekar console                 # Show recent console output
sidekar console errors          # Show only JS errors
sidekar cookies                 # List cookies for current page
sidekar cookies set <n> <v>     # Set a cookie
sidekar cookies delete <name>   # Delete a cookie
sidekar cookies clear           # Clear all cookies
sidekar storage get [key]       # Show localStorage/sessionStorage
sidekar storage set <key> <val> # Set a storage item
sidekar storage clear [target]  # Clear storage (local, session, all, everything)
sidekar network capture [secs]  # Capture XHR/fetch requests
sidekar network show [filter]   # Re-display last capture
sidekar sw list                 # List service workers
sidekar sw unregister           # Unregister all service workers
```

### Page environment

```bash
sidekar block <pattern>         # Block requests: images, css, fonts, media, scripts, or URL
sidekar block --ads             # Block ads, analytics, and tracking (40+ patterns)
sidekar block off               # Disable request blocking
sidekar viewport <preset|w h>   # Set viewport (mobile, tablet, desktop, iphone, ipad)
sidekar zoom [level]            # Zoom page (in, out, 50, reset)
sidekar grid [spec]             # Overlay coordinate grid (off to remove)
sidekar media dark|light|print  # Emulate media features
sidekar animations pause|resume # Control animation playback
sidekar security ignore-certs   # Accept self-signed certificates
sidekar download path [dir]     # Set download directory
```

### Desktop automation (macOS)

```bash
sidekar desktop-apps            # List running applications
sidekar desktop-windows --app X # List windows for an app
sidekar desktop-find --app X <q> # Search UI elements by text
sidekar desktop-click --app X <q> # Click a UI element by text match
sidekar desktop-screenshot      # Capture full desktop or specific app
sidekar desktop-launch <app>    # Launch an application
sidekar desktop-activate --app X # Bring app to foreground
sidekar desktop-quit --app X    # Quit an app gracefully
```

### Monitoring

```bash
sidekar monitor start <tabs>    # Watch tabs for title/favicon changes
sidekar monitor stop            # Stop watching
sidekar monitor status          # Show watcher state
```

### Scheduling

```bash
sidekar cron_create             # Create a recurring scheduled job
sidekar cron_list               # List active cron jobs
sidekar cron_delete <id>        # Delete a cron job
```

Jobs execute sidekar tools on a cron schedule and deliver results via the agent bus. Persisted in SQLite across session restarts.

### TOTP (two-factor codes)

```bash
sidekar totp add <service> <account> <secret>  # Store a TOTP secret (base32)
sidekar totp list                               # List stored TOTP entries
sidekar totp get <service> <account>            # Print current 6-digit code only
sidekar totp remove <id>                        # Delete a stored secret
```

Useful for automated login flows that require two-factor authentication. Secrets are stored encrypted on disk.

### KV store (encrypted key-value storage)

```bash
sidekar kv set <key> <value>    # Store a value
sidekar kv get <key>            # Retrieve a value
sidekar kv list                 # List all keys
sidekar kv delete <key>         # Delete a key
```

Values are encrypted at rest (AES-256-GCM) when logged in to sidekar.dev.

Project-scoped keys are tied to the current working directory. Global keys are shared across all projects. Both persist across sessions in SQLite.

### Error log

```bash
sidekar errors                  # Show recent errors (default 10)
sidekar errors 25               # Show last 25 errors
```

Surfaces recent sidekar errors for debugging failed commands or transport issues.

### Batch execution

```bash
sidekar batch '<json>'          # Execute multiple actions sequentially
```

### Session management

```bash
sidekar lock [seconds]          # Lock tab for exclusive access (default 300s)
sidekar unlock                  # Release tab lock
```

### Configuration

```bash
sidekar config get              # Show current config
sidekar config set <key> <val>  # Set config (telemetry, feedback, browser, auto_update, cdp_timeout_secs)
sidekar install                 # Install SKILL.md into detected agent skill directories
sidekar update                  # Check for and apply updates
sidekar feedback <rating> [txt] # Send feedback (1-5)
```

**Ref-based targeting:** After `axtree -i`, `observe`, or `text`, use the ref numbers directly as selectors (`click 1`, `type 3 hello`). Cached per URL with 48-hour TTL.

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
│  │  core · data · interaction · session · desktop            │ │
│  │  batch · monitor · cron · totp · kv · errors              │ │
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
- **CLI** (`main.rs` -> `commands/mod.rs`): Command dispatch for browser, desktop, bus, monitor, cron, etc.
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
| **What it is** | Rust binary: CDP + extension bridge, desktop, agent bus, monitor/cron | SDK / CLI wrapping Playwright (often via MCP) |
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
| `telemetry` | `true` | Anonymous usage stats (tool counts, session duration) |
| `feedback` | `true` | Allow feedback prompt after extended sessions |
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
├── ext.rs               # Chrome extension bridge (ext-server)
├── commands/
│   ├── mod.rs           # Command dispatch table (~80 commands)
│   ├── core.rs          # launch, connect, navigate, read, text, dom, axtree,
│   │                    #   screenshot, click, type, press, tabs, search, readurls
│   ├── data.rs          # cookies, console, network, block, viewport, zoom,
│   │                    #   frames, media, animations, security, storage, sw
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
├── scripts.rs           # Embedded JS scripts (page brief, DOM extract, axtree, etc.)
├── types.rs             # SessionState, DebugTab, InteractiveElement, etc.
├── utils.rs             # Browser detection, key mapping, file helpers
├── config.rs            # SidekarConfig (JSON in ~/.config/sidekar/)
├── api_client.rs        # Telemetry, feedback, version check, self-update
└── auth.rs              # Device auth flow (GitHub OAuth)

relay/                   # Relay server (Fly.io deployed, WSS + bus fan-out)
www/                     # Landing page + API (Vercel)
extension/               # Chrome extension (MV3)
```

## License

MIT
