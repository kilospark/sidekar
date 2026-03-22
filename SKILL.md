---
name: sidekar
description: Use when the user asks to interact with a website, browse the web, check a site, send a message, read content from a web page, or accomplish any goal that requires controlling a browser
---

# Sidekar Browser Control

Control Chrome directly via the Chrome DevTools Protocol. Raw CDP through a CLI helper.

**If you have sidekar MCP tools available (e.g. `navigate`, `click`), stop here and use those instead.** The MCP server handles session management and tab isolation automatically. The rest of this file is for CLI-only environments where MCP tools are not available.

## How to Run Commands

All commands use the `sidekar` CLI (the `sidekar` binary). Use the binary on PATH.

### Session Setup (once)

```bash
sidekar launch
```

This launches Chrome (or connects to an existing instance) and creates a session. All subsequent commands auto-discover the session — no session ID needed. Use `--headless` for invisible operation. Use `--tab <id>` to target a specific tab (creates an isolated session to avoid polluting the original).

### Running Commands

Use direct CLI commands. Each is a single bash call:

```bash
sidekar navigate https://example.com
sidekar click button.submit
sidekar keyboard "hello world"
sidekar press Enter
sidekar dom
```

**Auto-brief:** State-changing commands (navigate, click, hover, press Enter/Tab, scroll, select, waitfor) auto-print a compact page summary showing URL, title, inputs, buttons, links, and total element counts. Read it first. Do not take a screenshot after every action. Use `axtree -i` or `observe` when you need actionable elements, `read` for content, `dom` only when you need HTML structure.

### Command Reference

| Command | Example |
|---------|---------|
| `launch [options]` | `sidekar launch` or `sidekar launch --headless` or `sidekar launch --profile bot` |
| `navigate <url>` | `sidekar navigate https://example.com` |
| `kill` | `sidekar kill` |
| `batch <json>` | `sidekar batch '{"actions": [{"tool": "click", "target": "..."}]}'` |
| `grid [spec]` | `sidekar grid` or `sidekar grid 8x6` or `sidekar grid off` |
| `install` | `sidekar install` |
| `media <features>` | `sidekar media dark` or `sidekar media reset` |
| `animations <action>` | `sidekar animations pause` or `sidekar animations resume` |
| `security <action>` | `sidekar security ignore-certs` or `sidekar security strict` |
| `storage <action>` | `sidekar storage clear everything` or `sidekar storage get` |
| `sw <action>` | `sidekar sw unregister` or `sidekar sw list` |
| `back` | `sidekar back` |
| `forward` | `sidekar forward` |
| `reload` | `sidekar reload` |
| `feedback <1-5> [text]` | `sidekar feedback 5 "Works great!"` |
| `read [selector] [--tokens=N]` | `sidekar read` or `sidekar read article` or `sidekar read --tokens=2000` |
| `text [selector] [--tokens=N]` | `sidekar text` or `sidekar text --tokens=2000` |
| `dom [selector] [--tokens=N]` | `sidekar dom` or `sidekar dom .results` or `sidekar dom --tokens=1000` |
| `axtree [selector] [-i]` | `sidekar axtree` or `sidekar axtree -i` |
| `observe` | `sidekar observe` |
| `screenshot [options]` | `sidekar screenshot` or `sidekar screenshot --ref=3` or `sidekar screenshot --selector=.main --scale=1` |
| `fill <sel val ...>` | `sidekar fill "#email" "user@example.com" "#pass" "secret"` |
| `pdf [path]` | `sidekar pdf` or `sidekar pdf /tmp/page.pdf` |
| `click <sel\|x,y\|--text>` | `sidekar click button.submit` or `click 550,197` or `click --text Close` |
| `click --mode=double <sel\|x,y\|--text>` | `sidekar click --mode=double td.cell` or `click --mode=double 550,197` |
| `click --mode=right <sel\|x,y\|--text>` | `sidekar click --mode=right .context-target` or `click --mode=right 550,197` |
| `hover <sel\|x,y\|--text>` | `sidekar hover .menu-trigger` or `hover --text Settings` |
| `focus <selector>` | `sidekar focus input[name=q]` |
| `clear <selector>` | `sidekar clear input[name=q]` |
| `type <selector> <text>` | `sidekar type input[name=q] search query` |
| `keyboard <text>` | `sidekar keyboard hello world` |
| `paste <text>` | `sidekar paste Hello world` |
| `select <selector> <value>` | `sidekar select select#country US` |
| `upload <selector> <file>` | `sidekar upload input[type=file] /tmp/photo.png` |
| `drag <from> <to>` | `sidekar drag .card .dropzone` |
| `dialog <accept\|dismiss> [text]` | `sidekar dialog accept` |
| `waitfor <selector> [ms]` | `sidekar waitfor .dropdown 5000` |
| `waitfornav [ms]` | `sidekar waitfornav` |
| `press <key\|combo>` | `sidekar press Enter` or `sidekar press Ctrl+A` |
| `scroll <target> [px]` | `sidekar scroll down 500` or `sidekar scroll top` |
| `eval <js>` | `sidekar eval document.title` |
| `cookies [get\|set\|clear\|delete]` | `sidekar cookies` or `sidekar cookies set name val` |
| `console [show\|errors\|listen]` | `sidekar console` or `sidekar console errors` |
| `network [capture\|show]` | `sidekar network capture 10 api` or `sidekar network show cloudwatch` |
| `block <pattern>` | `sidekar block images css` or `sidekar block off` |
| `viewport <w> <h>` | `sidekar viewport mobile` or `sidekar viewport 1024 768` |
| `zoom <level>` | `sidekar zoom 50` or `sidekar zoom out` or `sidekar zoom reset` |
| `frames` | `sidekar frames` |
| `frame <id\|selector>` | `sidekar frame main` or `sidekar frame iframe#embed` |
| `download [path\|list]` | `sidekar download path /tmp/dl` or `sidekar download list` |
| `tabs` | `sidekar tabs` |
| `tab <id>` | `sidekar tab ABC123` |
| `newtab [url]` | `sidekar newtab https://example.com` |
| `close` | `sidekar close` |
| `activate` | `sidekar activate` |
| `minimize` | `sidekar minimize` |
| `resolve <selector>` | `sidekar resolve a.apply-btn` or `sidekar resolve 3` |
| `find <query>` | `sidekar find "submit button"` |
| `update` | `sidekar update` |
| `search <query> [--engine=E]` | `sidekar search "best restaurants"` or `sidekar search "query" --engine=duckduckgo` |
| `readurls <url ...>` | `sidekar readurls https://a.com https://b.com` |
| `connect` | `sidekar connect` |
| `run <sid>` | `sidekar run a1b2c3d4` |
| `click --mode=human <sel\|x,y>` | `sidekar click --mode=human button.submit` |
| `type --human <sel> <text>` | `sidekar type --human input[name=q] hello` |
| `lock [seconds]` | `sidekar lock 30` |
| `unlock` | `sidekar unlock` |
| `uninstall` | `sidekar uninstall` |
| `mcp` | `sidekar mcp` (run as MCP server via stdio) |
| `clipboard <html\|text>` | `sidekar clipboard --html="<b>bold</b>" --text="bold"` |
| `inserttext <text>` | `sidekar inserttext "large block of text"` |
| `config <get\|set> [key] [value]` | `sidekar config get` or `sidekar config set telemetry false` |

**`type` vs `keyboard` vs `paste` vs `clipboard` vs `inserttext`:** Use `type` to focus a specific input and fill it. Use `keyboard` to type at the current caret position — essential for rich text editors (Slack, Google Docs, Notion) where `type`'s focus call resets the cursor. Use `paste` to insert text via a ClipboardEvent — works with apps that intercept paste and is faster than `keyboard` for large text. Use `clipboard` to paste HTML/rich text via real clipboard API + Cmd+V — works with Google Docs, Sheets, Notion. Use `inserttext` for fast plain text insertion at cursor via CDP `Input.insertText`.

**`click` behavior:** Prefer refs from `axtree -i` or `observe`. Otherwise use a CSS selector or `--text`. Waits up to 5s for the element, scrolls it into view, then clicks. When multiple elements match `--text`, interactive elements (button, a, input, [role=button]) are preferred over generic containers (div, span). Use coordinates from a screenshot only as a last resort for canvas/image/iframe-heavy pages where ref, text, and selector targeting have all failed.

**`fill`:** Fill multiple form fields in one call. Pass alternating selector/value pairs: `fill "#email" "user@example.com" "#password" "secret"`. More efficient than multiple `type` calls. Supports ref numbers from `axtree -i`.

**`screenshot` options:** Expensive (~500+ vision tokens). Defaults to 800px wide JPEG for token efficiency. Use `--ref=N` to crop to a ref number from `axtree -i` (cheapest visual option), `--selector=CSS` to crop to an element, `--scale=1` for full viewport resolution (or any multiplier), `--format=png` for lossless, `--quality=N` (1-100), `--pad=N` to control padding around ref/selector crops (default: 48).

**`dialog` behavior:** Sets a one-shot auto-handler. Run BEFORE the action that triggers the dialog.

**`read`:** Reader-mode text extraction. Strips navigation, sidebars, ads, and returns just the main content as clean text with headings, lists, and paragraphs. Best for articles, docs, search results, and information retrieval.

**`text`:** Full page in reading order, interleaving static text with interactive elements (numbered refs). Like a screen reader view. Generates ref map as side effect, so you can use ref numbers in click/type/etc afterward. Best for complex pages where you need both content and interaction targets.

**`axtree` vs `dom`:** The accessibility tree shows semantic roles (button, link, heading, textbox) and accessible names - better for understanding page structure. Use `dom` when you need HTML structure/selectors; use `axtree` when you need to understand what's on the page.

**`axtree -i` (interactive mode):** Shows only actionable elements (buttons, links, inputs, etc.) as a flat numbered list. Most token-efficient way to see what you can interact with on a page. After running `axtree -i`, use the ref numbers directly as selectors: `click 1`, `type 3 hello`. Refs are cached per URL and reused on revisits.

**`observe`:** Like `axtree -i` but formats each element as a ready-to-use command (e.g. `click 1`, `type 3 <text>`, `select 5 <value>`). Generates the ref map as a side effect.

**Ref-based targeting:** After `axtree -i` or `observe`, numeric refs work in all selector-accepting commands: `click`, `type`, `select`, `hover`, `focus`, `clear`, `upload`, `drag`, `waitfor`, `dom`.

**`press` combos:** Supports modifier keys: `Ctrl+A` (select all), `Ctrl+C` (copy), `Meta+V` (paste on Mac), `Shift+Enter`, etc. Modifiers: Ctrl, Alt, Shift, Meta/Cmd.

**Mac keyboard note:** On macOS, app shortcuts documented as `Ctrl+Alt+<key>` (e.g., Google Docs heading shortcuts `Ctrl+Alt+1` through `Ctrl+Alt+6`) must be sent as `Meta+Alt+<key>` through CDP. Mac's Ctrl key is not the Command key these apps expect. Example: `press Meta+Alt+2` for Heading 2 in Google Docs.

**`scroll` targets:** `up`/`down` (default 400px, or specify pixels), `top`/`bottom`, or a CSS selector to scroll an element into view. **Element-scoped:** `scroll <selector> <up|down|top|bottom> [px]` scrolls within a container element instead of the page — essential for apps with custom scroll containers (Google Docs, Slack).

**`network` capture:** Captures XHR/fetch/API requests for a duration. `network capture 10` captures for 10 seconds. `network capture 15 api/query` captures for 15s, filtering to URLs containing "api/query". `network show` re-displays the last capture. `network show cloudwatch` filters saved results. Shows method, URL, status, type, timing, and POST body. Essential for diagnosing API issues in SPAs.

**`block` patterns:** Block resource types (`images`, `css`, `fonts`, `media`, `scripts`) or URL substrings. Speeds up page loads. Use `block off` to disable.

**`viewport` presets:** `mobile` (375x667), `iphone` (390x844), `ipad` (820x1180), `tablet` (768x1024), `desktop` (1280x800). Or specify exact width and height.

**`frames`:** Lists all frames/iframes on the page. Use `frame <id>` to switch context, `frame main` to return to the top frame.

**Profiles:** Use profiles to launch isolated browser instances with separate data.
- `sidekar launch` uses the default shared profile.
- `sidekar launch --profile shopping-bot` creates or reuses a named profile.
- `sidekar launch --profile new` auto-generates a profile ID and returns it.
- Each profile runs its own browser process on its own port. Custom profiles can be killed with `sidekar kill`.

**`batch`:** Execute multiple actions sequentially in one call. Use a JSON array of actions. Smart waits are applied after successful state-changing actions (`navigate`, `click`, `fill`, `select`, `type`). Batch stops on the first non-optional error.
```bash
sidekar batch '{"actions": [{"tool": "click", "target": "--text Submit"}, {"tool": "waitfornav"}]}'
```
- Add `"retries": N` and `"retry_delay": ms` to retry flaky steps.
- Add `"optional": true` for dismissals or branches that can fail without aborting the batch.
- Add `"wait": ms` to override the post-step smart wait for a specific action.

**`grid`:** Overlay a coordinate grid for targeting elements in canvas/image-heavy apps. Each cell displays its center coordinate.
- `sidekar grid` (default 10x10)
- `sidekar grid 8x6` (cols x rows)
- `sidekar grid 50` (50px cell size)
- `sidekar grid off` (remove overlay)

**`install`:** Register sidekar as an MCP server with all detected clients (Claude Code, Cursor, Windsurf, Claude Desktop, etc.) without re-downloading the binary.

**Troubleshooting SPAs and Stale Pages:**
- **`sw unregister`**: remove service workers that cache old content.
- **`storage clear everything`**: clear all storage, caches, cookies, and service workers for the origin.
- **`reload`**: force a fresh page load.

**Media and Animations:**
- **`media dark`**: switch to dark color scheme.
- **`media reset`**: restore defaults.
- **`animations pause`**: freeze JS animations (sets playback rate to 0).
- **`animations resume`**: restore normal playback.
- **`security ignore-certs`**: accept self-signed certificates for the current origin.

### Tab Isolation

Each session creates and owns its own tabs. Sessions never reuse tabs from other sessions or pre-existing tabs.

- `launch`/`connect` creates a **new blank tab** for the session
- `newtab` opens an additional tab within the session
- `tabs` only lists tabs owned by the current session
- `tab <id>` only switches to session-owned tabs
- `close` removes the tab from the session
- Clicks that open a new tab via `target=_blank` or `window.open` are auto-adopted into your session and become the active tab

This means two agents can work side by side in the same Chrome instance without interfering with each other.

**Shared Chrome awareness:** When multiple agents share Chrome, link clicks on sites like Slack can hijack your tab (e.g. Slack's link unfurling navigates to Jira). Always record your tab ID after `launch`/`newtab` and verify you're on the right tab before acting. If your tab's URL has changed unexpectedly, use `tab <id>` to switch back or `tabs` to audit your session.

## The Perceive-Act Loop

1. **PLAN** — Break the goal into steps.
2. **ACT** — Run the appropriate command. State-changing commands auto-print a page brief.
3. **DECIDE** — Read the brief. If you need more, use the cheapest sufficient perception tool (see escalation order below).
4. **REPEAT** until done or blocked.

## Rules

<HARD-RULES>

1. **Read the brief after acting.** State-changing commands auto-print a page brief. Read it before deciding your next step.

2. **Text tools before screenshot.** Only use `screenshot` when the page is canvas/image-heavy, you need visual verification, or text tools are insufficient. Start with `--ref=N` or `--selector` crops — not full page.

3. **Report actual content.** When the goal is information retrieval, extract and present the actual text from the page. Do not summarize — show what IS there.

4. **Stop when blocked.** If you encounter a login wall, CAPTCHA, 2FA, or cookie consent, run `activate` to bring the browser to front, then tell the user. Do not guess credentials.

5. **Wait for dynamic content.** After clicks that trigger page loads, use `waitfornav` or `waitfor <selector>` before reading DOM.

6. **Prefer ref-based targeting.** Use refs from `axtree -i`, `observe`, or `text`. Use CSS selectors when you need DOM structure or a ref is unavailable. Use coordinates only as a last resort for canvas/iframe surfaces.

7. **Clean up tabs.** Close tabs opened with `newtab` when done. Run `tabs` before reporting completion.

8. **Track tab IDs.** Note tab IDs from `launch`/`newtab` output. Verify you're on the expected tab before acting.

</HARD-RULES>

## Perception Escalation

Stop at the first tool that gives you what you need:

| Priority | Tool | Use for | Cost |
|----------|------|---------|------|
| 1 | `read` | Page content (articles, docs, search results) | Low |
| 2 | `axtree -i` / `observe` | Actionable elements with refs | Low |
| 3 | `text` | Full visible text + refs (cap with `--tokens=N`) | Low-Med |
| 4 | `dom` | HTML structure/selectors (scope with selector or `--tokens=N`) | Medium |
| 5 | `screenshot --ref=N` or `--selector` | Visual of one element (800px wide) | Medium |
| 6 | `screenshot` | Full page visual fallback (800px wide) | High |
| 7 | `zoom out` then `screenshot` | More content per screenshot | High |
| 8 | `screenshot --scale=1` | Full viewport resolution (last resort) | Highest |

## Targeting Elements (priority order)

1. **refs**: from `axtree -i`, `observe`, or `text` — `click 3`, `type 5 hello`, `screenshot --ref=7`
2. **text search**: `click --text Submit` — finds the smallest visible text match, then clicks the nearest actionable ancestor (button/link/tab/etc.) when needed
3. **CSS selectors**: `#id`, `[data-testid="..."]`, `[aria-label="..."]`, `.class`, structural
4. **eval**: `eval` with querySelector when the element is present but hard to target
5. **coordinates**: `click 550,197` — last resort for canvas/iframes only, after all above have failed

## Common Patterns

All examples assume you've already run `sidekar launch`.

**Navigate and read** (navigate auto-prints brief - no separate dom needed):
```bash
sidekar navigate https://news.ycombinator.com
```

**Fill a form:**
```bash
# Multiple fields at once:
sidekar fill "input[name=q]" "search query"
# Or one at a time:
sidekar click input[name=q]
sidekar type input[name=q] search query
sidekar press Enter
```

**Rich text editors and @mentions:**
```bash
sidekar click .ql-editor
sidekar keyboard Hello @alice
sidekar waitfor [data-qa='tab_complete_ui_item'] 5000
sidekar click [data-qa='tab_complete_ui_item']
sidekar keyboard " check this out"
```

## Complex Web Apps

**Portals, shadow DOM, and overlays:**
- Modal dialogs, dropdowns, and popups often render in portal containers — CSS selectors from parent context won't find them
- `axtree -i` and `observe` include deep overlays, nested menus, and portal content — try refs first
- `click --text` finds elements inside portals and across shadow DOM boundaries, then walks up to the nearest actionable ancestor before clicking
- `dom` traverses open shadow roots — web component internals are visible
- When all else fails, use `eval` to find and `.click()` directly
- Coordinate clicks from screenshots are a last resort for canvas/iframe-only surfaces

## Configuration

Settings file: `~/.config/sidekar/sidekar.json`

```json
{
  "telemetry": true,
  "feedback": true
}
```

Set `telemetry` to `false` to opt out of anonymous usage statistics (tool counts per session, no PII). Edit the file directly or use `sidekar config set <key> <true|false>`.
