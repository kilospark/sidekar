# sidekar: the sidecar for AI agents

**Browser automation** (Chrome DevTools Protocol plus the optional Chrome extension), **desktop automation** on macOS, **inter-agent communication and orchestration** via a local bus, and **background automation** (tab monitoring and cron). Ships as a single Rust binary with zero runtime dependencies. Works with Claude Code, Codex, Cursor, Copilot, Gemini CLI, OpenCode, and other agents through the bundled skill (`sidekar install`) or the [Vercel skills registry](https://github.com/vercel-labs/skills).

No Playwright, no browser automation frameworks. Raw CDP over WebSocket.

## Install

### Recommended

```bash
curl -fsSL https://sidekar.dev/install | sh
```

Downloads the `sidekar` binary, adds it to your `PATH`, and runs `sidekar install` to place `SKILL.md` into each detected agent’s skills directory (Claude Code, Codex, Gemini CLI, OpenCode, Pi, etc.).

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

Or copy `SKILL.md` from this repo into your agent’s skills folder (see output of `sidekar install` when no agents are detected).

## Step-by-step

1. **Install sidekar.** Download the binary and install the agent skill in one step:
   ```bash
   curl -fsSL https://sidekar.dev/install | sh
   ```
   This adds `sidekar` to your `PATH` and runs `sidekar install` so `SKILL.md` is copied into each detected agent’s skills directory.

2. **Chrome extension (optional).** To drive your everyday Chrome profile (same cookies and logins as the window you already use), load the MV3 extension from the `extension/` directory, start the bridge (`sidekar ext-server` or any `sidekar ext …` command), paste the shared secret in the extension popup, and connect. See [`extension/README.md`](extension/README.md) for full steps.

3. **`sidekar login` (optional).** Run `sidekar login` to sign in with sidekar.dev and store a device token. Use this when you want remote access to sessions (for example managing or attaching to sessions from the web dashboard) instead of only local use.

4. **Launch an agent with sidekar.** From a terminal, run `sidekar <agent> [args…]` where `<agent>` is any agent CLI on your `PATH` or a shell alias (for example `sidekar claude`, `sidekar codex`). Sidekar wraps the process in a PTY, registers it on the local agent bus, and wires browser and bus tooling for that session.

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

These are the four capability pillars. Everything else (for example web search, multi-page reads, batch runs) is a **use case** on top of them.

### 1. Browser automation (CDP + Chrome extension)

Full Chrome DevTools Protocol access via raw WebSocket: navigate, click, type, forms, dialogs, cookies, network capture, screenshots, PDFs. The optional **Chrome extension** extends the same automation story where an in-page bridge helps (see `extension/` and `sidekar ext-server`). Token-efficient perception returns compact page summaries instead of raw DOM dumps. Real-browser search (`search`), parallel URL reads (`readurls`), and similar flows are **uses** of this pillar, not separate products.

### 2. Desktop automation (macOS)

Control native applications via the macOS Accessibility API: find and click UI elements, desktop screenshots, launch and quit apps, list windows. Runs without Chrome when you need native UI, including permission dialogs and surfaces outside CDP-driven pages.

### 3. Inter-agent communication and orchestration

Agents discover and coordinate via a **local bus** (SQLite broker, Unix sockets): `who`, `bus_send`, `bus_done`, handoffs, and durable message tracking. Multi-terminal workflows often use the PTY helpers (`sidekar claude`, `sidekar codex`, …) for registration and I/O; that is **how** you run agents, not a separate user-facing pillar.

### 4. Background automation

**Monitor:** watch tab titles and favicons, debounced, with notifications routed through the bus. **Cron:** run tools on a schedule (standard cron expressions), persist jobs in SQLite, deliver results to agents. Together they cover unattended and reactive work.

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

### Agent bus

```bash
sidekar who                     # List agents on your channel
sidekar bus_send <to> <message> # Send a message to another agent
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

Each agent gets automatic bus registration, a unique nickname shown in the terminal title, and input injection via Unix sockets. 

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
┌─────────────────────────────────────────────────────┐
│  sidekar binary (Rust)                              │
│                                                     │
│  ┌───────────┐  ┌──────────┐  ┌──────────────────┐ │
│  │ Skill      │  │   CLI    │  │   PTY Wrapper    │ │
│  │ install    │  │ dispatch │  │ (fork+exec)      │ │
│  │ (skill.rs) │  │          │  │                  │ │
│  └─────┬──────┘  └────┬─────┘  └────┬─────────────┘ │
│        │              │              │               │
│  ┌─────▼──────────────▼──────────────▼─────────────┐ │
│  │              Command Dispatch                    │ │
│  │  core · data · interaction · session · desktop   │ │
│  │  batch · monitor · cron                          │ │
│  └─────┬──────────────┬──────────────┬─────────────┘ │
│        │              │              │               │
│  ┌─────▼──────┐ ┌─────▼──────┐ ┌────▼────────────┐ │
│  │  CDP Client │ │  Agent Bus │ │ Desktop (macOS) │ │
│  │  WebSocket  │ │  SQLite    │ │ Accessibility   │ │
│  │  to Chrome  │ │  Broker    │ │ API + Screen    │ │
│  └─────────────┘ └────────────┘ └─────────────────┘ │
│                                                     │
│  ┌──────────┐  ┌─────────┐  ┌───────────────────┐  │
│  │ Extension│  │ Tunnel/ │  │  Telemetry/       │  │
│  │ bridge   │  │ Relay   │  │  Auto-update      │  │
│  │ (ext.rs) │  │         │  │                   │  │
│  └──────────┘  └─────────┘  └───────────────────┘  │
└─────────────────────────────────────────────────────┘
```

- **Skill installer** (`skill.rs`): Copies `SKILL.md` into detected agent skill directories; `sidekar install` entry point
- **CLI** (`main.rs` → `commands/mod.rs`): Command dispatch for browser, desktop, bus, monitor, cron, etc.
- **PTY Wrapper** (`pty.rs`): Fork+exec agents in a PTY, register on bus, bridge I/O, signal forwarding
- **CDP Client** (`lib.rs`): Raw WebSocket to Chrome's debug port, request/response matching, event queue, auto-dialog handling, connection retry, TCP keepalive
- **Agent Bus** (`bus.rs` + `broker.rs` + `message.rs` + `transport.rs`): SQLite-backed agent registry, typed envelope protocol (request/response/fyi/done), delivery via SQLite message queue, nudge timers, timeout tracking
- **Desktop** (`desktop/`): macOS-only Accessibility API (`objc2-app-kit`), screen capture (`screencapturekit`), input simulation (`enigo`)
- **Monitor** (`commands/monitor.rs`): Background task watches tab titles/favicons via CDP, delivers notifications via bus transport. 

- **Tunnel** (`tunnel.rs`): WSS connection to relay server for remote session access, auto-reconnect with exponential backoff

## Token Stats

Each command is designed to minimize token usage while giving the agent enough context to decide its next step.

| Command | sidekar output | Playwright equivalent | Savings |
|---------|--------------|----------------------|---------|
| **brief** (auto) | ~200 chars | No equivalent - `page.content()` returns ~50k-500k chars | **~99%** |
| **read** | ~1k-4k chars (clean text) | No equivalent - manual extraction needed | - |
| **text** | ~1k-4k chars (text + refs) | `page.accessibility.snapshot()` ~10k-50k chars | **~90%** |
| **dom** | ~1k-4k chars (compact HTML) | `page.content()` ~50k-500k chars (full raw HTML) | **~95%** |
| **axtree -i** | ~500-1.5k chars (flat list) | `page.accessibility.snapshot()` ~10k-50k chars | **~95%** |

**Recommended flow for minimal token usage:**
1. State-changing commands auto-print the **brief** (~200 chars) - often enough to decide next step
2. Need to read page content? Use **read** - strips UI chrome, returns clean text
3. Need to see everything + interact? Use **text** - full page with refs
4. Need just interactive elements? Use **axtree -i** (~500 tokens)
5. Need HTML structure? Use **dom** with a selector to scope
6. Reserve **screenshot** for visual-heavy pages where text extraction is insufficient

## vs. Playwright-based tools

Several tools give AI agents browser control on top of Playwright: [agent-browser](https://github.com/vercel-labs/agent-browser) (Vercel), [Playwright MCP](https://github.com/microsoft/playwright-mcp) (Microsoft), [Stagehand](https://github.com/browserbase/stagehand) (Browserbase), and [Browser Use](https://github.com/browser-use/browser-use).

|  | **sidekar** | **Playwright-based tools** |
|--|-----------|--------------------------|
| **What it is** | Rust binary: CDP + extension bridge, desktop, agent bus, monitor/cron | SDK / CLI wrapping Playwright (often via MCP) |
| **Architecture** | Direct CDP WebSocket to your Chrome | CLI/SDK → IPC → Playwright → bundled Chromium |
| **Install size** | Single binary, zero deps | ~200 MB+ (node_modules + Chromium download) |
| **Uses your browser** | Yes - your Chrome, your cookies, your logins | No - launches bundled Chromium with clean state |
| **User agent** | Your real Chrome user agent | Modified Playwright/Chromium UA - detectable |
| **Headed mode** | Always (unless `--headless`) - you see what the agent sees | Headless by default |
| **Multi-agent** | Built-in bus for agent discovery and messaging | None |
| **Desktop control** | macOS Accessibility API integration | None |

### Token comparison (same pages, measured output)

| Scenario | **sidekar** | **Playwright-based*** | Savings |
|----------|-----------|------------------|---------|
| **Navigate + see page** | `navigate` = 186 chars | `open` + `snapshot -i` = 7,974 chars | **98%** |
| **Navigate + see page** | `navigate` = 756 chars | `open` + `snapshot -i` = 8,486 chars | **91%** |
| **Full page read** | `read` = ~3,000 chars | No equivalent (manual extraction) | - |
| **Full page + refs** | `text` = ~4,000 chars | `snapshot` = 104,890 chars | **96%** |
| **Interactive elements** | `axtree -i` = 5,997 chars | `snapshot -i` = 7,901 chars | **24%** |

## Data directories

| Path | Purpose |
|------|---------|
| `~/.sidekar/` | Session state, action cache, tab locks, broker DB, Chrome profiles |
| `~/.sidekar/profiles/default/` | Default Chrome user data directory |
| `~/.sidekar/broker.sqlite3` | Agent bus registry and message tracking |
| `~/.config/sidekar/sidekar.json` | User configuration (telemetry, browser, timeouts) |
| `/tmp/sidekar-*` | Screenshots, PDFs, network captures, command files |

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
├── transport.rs         # Message delivery trait + SQLite broker transport
├── ipc.rs               # Legacy stub (sockets replaced by SQLite queue)
├── pty.rs               # PTY wrapper for launching agents (fork+exec)
├── tunnel.rs            # WSS tunnel client for relay server
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

relay/                   # Separate relay server (Fly.io deployed)
www/                     # Landing page + API (Vercel)
extension/               # Chrome extension (MV3)
```

## License

MIT
