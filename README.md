# sidekar - token-efficient browser control for AI agents

A highly token efficient browser control tool that lets you control any Chromium-based browser via the Chrome DevTools Protocol. Ships as a Rust binary with zero runtime dependencies. Works as an MCP server with Claude Code, Claude Desktop, Cursor, Codex, Windsurf, Cline, ChatGPT Desktop, and any MCP-compatible client. Also works as a CLI skill with Claude Code, Cursor, Codex, Windsurf, Cline, Copilot, OpenCode, Goose, and any tool supporting the [Agent Skills](https://agentskills.io) spec.

No Playwright, no browser automation frameworks. Raw CDP over WebSocket.

## Install

### MCP Server (recommended)

```bash
curl -fsSL https://sidekar.dev/install | sh
```

Downloads the `sidekar` binary and auto-configures any detected MCP clients (Claude Desktop, Claude Code, ChatGPT Desktop, Cursor, Windsurf, Cline, Codex).

### Uninstall

```bash
curl -fsSL https://sidekar.dev/uninstall | sh
```

### Agent Skill

```bash
npx skills add kilospark/sidekar
```

Works with Claude Code, Cursor, Codex, Windsurf, Cline, Copilot, OpenCode, Goose, and [40+ agents](https://github.com/vercel-labs/skills). Powered by Vercel's [skills](https://github.com/vercel-labs/skills) CLI.

### Manual MCP config

```json
{
  "mcpServers": {
    "sidekar": {
      "command": "sidekar",
      "args": ["mcp"]
    }
  }
}
```

For Claude Code:

```bash
claude mcp add -s user sidekar sidekar -- mcp
```

## Usage

Just tell your agent what you want:

```
check the top stories on Hacker News
navigate to github.com and show my notifications
search google for "best restaurants near me"
```

Or describe any goal - the agent will figure out the steps.

## How it works

The agent follows a **perceive-act loop**:

1. **Plan** - break the goal into steps
2. **Act** - navigate, click, type via CDP commands
3. **Perceive** - read the page to see what happened
4. **Decide** - adapt, continue, or report results
5. **Repeat** - until the goal is done

## Reading the page

sidekar provides multiple ways to read page content, each optimized for different needs:

| Need | Tool | Output |
|------|------|--------|
| Page content (articles, docs) | `read` | Clean text, no UI chrome |
| Full page + interaction targets | `text` | Text + numbered refs |
| Interactive elements only | `axtree -i` | Flat list of clickable/typeable elements |
| HTML structure/selectors | `dom` | Compact HTML |
| Visual layout | `screenshot` | PNG image |

**`read`** strips navigation, sidebars, ads, and returns just the main content as clean text with headings, lists, and paragraphs. Best for articles, docs, search results, and information retrieval.

**`text`** shows the full page in reading order, interleaving static text with interactive elements (numbered refs). Like a screen reader view. Generates a ref map so you can immediately use `click 12` or `type 3 hello`.

## Sessions

Each agent invocation gets its own **session** with isolated tab tracking. On `launch`, a unique session ID is generated and a fresh Chrome tab is created for that session.

- Multiple agents can work side by side in the same Chrome instance
- Each session only sees and controls its own tabs

## CLI

The `sidekar` CLI wraps CDP:

```bash
sidekar launch                  # Start browser, create session
sidekar navigate <url>          # Go to a URL (auto-dismisses cookie banners)
sidekar read [selector]         # Reader-mode text extraction (strips nav/sidebar/ads)
sidekar text [selector]         # Full page in reading order with interactive refs
sidekar dom [selector]          # Get compact DOM HTML
sidekar dom --tokens=N          # Truncate DOM to ~N tokens
sidekar axtree                  # Get accessibility tree (auto-capped at ~4k tokens)
sidekar axtree -i               # Interactive elements with ref numbers
sidekar axtree -i --diff        # Show only changes since last snapshot
sidekar observe                 # Interactive elements as ready-to-use commands
sidekar find <query>            # Find element by description
sidekar screenshot              # Capture screenshot
sidekar pdf [path]              # Save page as PDF
sidekar click <sel|x,y|--text>  # Click by selector, coordinates, or text match
sidekar click --mode=double <sel> # Double-click
sidekar click --mode=right <sel>  # Right-click (context menu)
sidekar hover <sel>             # Hover (tooltips/menus)
sidekar focus <selector>        # Focus an element without clicking
sidekar clear <selector>        # Clear an input field
sidekar type <selector> <text>  # Type into an input (focuses first)
sidekar keyboard <text>         # Type at current caret position (no selector)
sidekar inserttext <text>       # Insert text in one shot (fast, no formatting)
sidekar paste <text>            # Paste via clipboard event
sidekar clipboard --html <html> # Paste rich HTML via real clipboard (Google Docs, Notion)
sidekar fill <fields_json>      # Fill multiple form fields at once
sidekar select <sel> <value>    # Select option(s) from a dropdown
sidekar upload <sel> <file>     # Upload file(s) to a file input
sidekar click --mode=human <sel> # Click with human-like mouse movement
sidekar type --human <sel> <text> # Type with variable delays
sidekar drag <from> <to>        # Drag from one selector to another
sidekar dialog accept|dismiss   # Handle alert/confirm/prompt dialogs
sidekar waitfor <sel> [ms]      # Wait for element to appear (default 5s)
sidekar waitfornav [ms]         # Wait for navigation to complete (default 10s)
sidekar press <key>             # Press a key or combo (Enter, Ctrl+A, Meta+C)
sidekar scroll <target> [px]    # Scroll: up, down, top, bottom, or selector
sidekar eval <js>               # Run JavaScript in page context
sidekar cookies                 # List cookies for current page
sidekar cookies set <n> <v>     # Set a cookie
sidekar cookies delete <name>   # Delete a cookie
sidekar cookies clear           # Clear all cookies
sidekar console                 # Show recent console output
sidekar console errors          # Show only JS errors
sidekar block <pattern>         # Block requests: images, css, fonts, media, scripts, or URL
sidekar block --ads             # Block ads, analytics, and tracking (40+ patterns)
sidekar block off               # Disable request blocking
sidekar viewport <preset|w h>   # Set viewport (mobile, tablet, desktop, iphone, ipad)
sidekar frames                  # List all frames/iframes
sidekar frame <id|sel>          # Switch to a frame
sidekar frame main              # Return to main frame
sidekar tabs                    # List this session's tabs
sidekar tab <id>                # Switch to a session-owned tab
sidekar newtab [url]            # Open a new tab in this session
sidekar close                   # Close current tab
sidekar search <query>          # Search the web (Google, Bing, DuckDuckGo, or custom)
sidekar readurls <url1> <url2>  # Read multiple URLs in parallel
sidekar resolve <sel>           # Get link/form target URL without clicking
sidekar download <url> [path]   # Download a file from URL
sidekar network capture [secs]  # Capture XHR/fetch requests
sidekar zoom [level]            # Zoom page (in, out, 50, reset)
sidekar lock                    # Lock session to prevent concurrent access
sidekar unlock                  # Unlock session
sidekar batch <json>            # Execute multiple actions sequentially
sidekar grid [spec]             # Overlay coordinate grid (off to remove)
sidekar setup                   # Configure MCP clients without re-download
sidekar install                 # Install sidekar for a specific MCP client
sidekar config [key] [value]    # Get or set sidekar configuration
sidekar kill                    # Kill custom profile session
sidekar media <features>        # Emulate media (dark, light, print, reset)
sidekar animations <action>     # Control animations (pause, resume)
sidekar security <action>       # Control security (ignore-certs, strict)
sidekar storage <action>        # Manage storage (clear, get, set, remove)
sidekar sw <action>             # Manage service workers (list, unregister)
sidekar back / forward / reload # Navigation history
sidekar activate                # Bring browser window to front (macOS)
sidekar minimize                # Minimize browser window (macOS)
sidekar feedback <rating> [txt] # Send feedback (1-5)
```

**Ref-based targeting:** After `axtree -i`, `observe`, or `text`, use the ref numbers directly as selectors - `click 1`, `type 3 hello`. Cached per URL.

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
| **What it is** | Rust binary - MCP server + CLI | CLI / MCP server / SDK wrapping Playwright |
| **Architecture** | Direct CDP WebSocket to your Chrome | CLI/SDK &rarr; IPC &rarr; Playwright &rarr; bundled Chromium |
| **Install size** | Single binary, zero deps | ~200 MB+ (node_modules + Chromium download) |
| **Uses your browser** | Yes - your Chrome, your cookies, your logins | No - launches bundled Chromium with clean state |
| **User agent** | Your real Chrome user agent | Modified Playwright/Chromium UA - detectable |
| **Headed mode** | Always - you see what the agent sees | Headless by default |

### Token comparison (same pages, measured output)

| Scenario | **sidekar** | **Playwright-based*** | Savings |
|----------|-----------|------------------|---------|
| **Navigate + see page** | `navigate` = 186 chars | `open` + `snapshot -i` = 7,974 chars | **98%** |
| **Navigate + see page** | `navigate` = 756 chars | `open` + `snapshot -i` = 8,486 chars | **91%** |
| **Full page read** | `read` = ~3,000 chars | No equivalent (manual extraction) | - |
| **Full page + refs** | `text` = ~4,000 chars | `snapshot` = 104,890 chars | **96%** |
| **Interactive elements** | `axtree -i` = 5,997 chars | `snapshot -i` = 7,901 chars | **24%** |

## Build from source

```bash
git clone https://github.com/kilospark/sidekar.git
cd sidekar
cargo build --release
# Binary: target/release/sidekar (CLI + MCP server via `sidekar mcp`)
```

## Requirements

- Any Chromium-based browser: Google Chrome, Microsoft Edge, Brave, Arc, Vivaldi, Opera, or Chromium
- No runtime dependencies (single Rust binary)

Auto-detected on macOS, Linux, Windows, and WSL. Set `CHROME_PATH` to override.

## License

MIT
