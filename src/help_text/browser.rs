pub fn get(command: &str) -> Option<&'static str> {
    Some(match command {
        "browser" => {
            "\
sidekar browser <subcommand> [args...]

  Browser automation via Chrome DevTools Protocol (CDP).

  Session / chrome:
    launch, connect, stealth, debug, navigate, back, forward, reload
    tabs, tab, new-tab, close, activate, minimize, kill, frames, frame
    screencast, sessions, run

  Extension (host Chrome):
    ext                               Full extension surface (history, watch, monitor, …)

  Read / observe:
    read, text, dom, ax-tree, observe, find, resolve, screenshot, pdf
    search, read-urls, grid

  Interact:
    batch, click, hover, focus, clear, type, fill, keyboard, paste
    clipboard, insert-text, select, upload, drag, dialog, wait-for
    wait-for-nav, press, scroll, eval, media, animations, zoom, lock
    unlock, mouse

  Data / env:
    cookies, console, network, block, viewport, download, storage
    service-workers, security, geo, state, auth

  Global flags (before `browser`):
    --profile <name>   Managed Chrome profile (auto-launch on first use)
    --host             Extension transport for CDP-overlapping subs (see --tab note)
    --tab <id>         Tab target — ID namespace depends on transport (see below)

  Tab IDs:
    Managed CDP (default): IDs from `sidekar browser tabs` (CDP target ids).
    --host or `browser ext`: IDs from `sidekar browser ext tabs` (Chrome extension tab ids).
    Do not mix namespaces — the same number can refer to different tabs.

  Examples:
    sidekar browser navigate example.com
    sidekar browser click 3
    sidekar browser ext tabs
    sidekar --host browser read
    sidekar --host --tab 123456789 browser click \"#submit\"
    sidekar --profile work browser navigate https://internal.app"
        }
        "navigate" => {
            "\
sidekar browser navigate <url> [--no-dismiss]

  Navigate the active tab to <url>. Automatically adds https:// if no scheme.
  Auto-dismisses cookie consent banners and common popups after load.
  Returns a page brief with URL, title, visible inputs, buttons, links.

  On first use, auto-launches managed Chrome with the 'default' profile.
  Pass --profile <name> to use a different managed profile, or --host to
  drive your already-running Chrome via the sidekar extension.

  Options:
    --no-dismiss   Skip automatic popup/banner dismissal

  Examples:
    sidekar browser navigate example.com
    sidekar browser navigate https://github.com/search?q=rust --no-dismiss
    sidekar --profile work browser navigate https://internal.app
    sidekar --host browser navigate https://news.example.com"
        }
        "click" => {
            "\
sidekar browser click <target> [--mode=double|right|human]

  Click an element. Waits up to 5s for it to appear, scrolls into view.

  Target types (in priority order):
    <ref>          Ref number from ax-tree -i, observe, or text (e.g. 3)
    --text <text>  Find by visible text, prefer interactive ancestors
    <selector>     CSS selector (#id, .class, [data-testid=...])
    <x>,<y>        Coordinates from screenshot (last resort)

  Modes:
    --mode=double  Double-click
    --mode=right   Right-click / context menu
    --mode=human   Bezier curve mouse movement for bot detection evasion

  On macOS, --text auto-falls back to Accessibility API for Chrome-native UI
  (permission dialogs, extension popups) if not found in DOM.

  Examples:
    sidekar browser click 3
    sidekar browser click --text \"Sign in\"
    sidekar browser click \"#submit-btn\"
    sidekar browser click --mode=double 5
    sidekar browser click 450,300"
        }
        "type" => {
            "\
sidekar browser type <selector> <text> [--human]

  Focus the element matching <selector> and type <text> into it.
  Clears existing content first.

  Options:
    --human   Human-like typing with variable delays and occasional typos

  Use 'keyboard' instead for rich text editors where focus resets cursor.

  Examples:
    sidekar browser type \"#search\" \"rust async\"
    sidekar browser type 5 \"hello world\"
    sidekar browser type --human \"#email\" \"user@example.com\""
        }
        "keyboard" => {
            "\
sidekar browser keyboard <text>

  Type text at the current caret position without focusing a new element.
  Essential for rich text editors (Slack, Google Docs, Notion) where
  'type' would reset the cursor position.

  Example:
    sidekar browser click \".editor\"
    sidekar browser keyboard \"Hello world\""
        }
        "fill" => {
            "\
sidekar browser fill <selector1> <value1> [selector2] [value2] ...

  Fill multiple form fields in one call. Alternating selector/value pairs.
  More efficient than multiple 'type' calls.

  Examples:
    sidekar browser fill \"#email\" \"user@example.com\" \"#password\" \"secret\"
    sidekar browser fill 3 \"Alice\" 5 \"alice@example.com\""
        }
        "read" => {
            "\
sidekar browser read [selector] [--tokens=N]

  Reader-mode text extraction. Strips navigation, sidebars, ads.
  Returns clean text with headings, lists, paragraphs.
  Best for articles, documentation, search results.

  Options:
    selector     CSS selector to scope extraction
    --tokens=N   Approximate token limit for output

  Examples:
    sidekar browser read
    sidekar browser read article --tokens=2000
    sidekar browser read \".main-content\""
        }
        "text" => {
            "\
sidekar browser text [selector] [--tokens=N]

  Full page text in reading order, interleaving static text with
  interactive elements (numbered refs). Like a screen reader view.
  Generates ref map as side effect.

  Best for complex pages where you need both content and interaction targets.

  Examples:
    sidekar browser text
    sidekar browser text --tokens=3000"
        }
        "ax-tree" => {
            "\
sidekar browser ax-tree [options] [selector]

  Accessibility tree — semantic roles and accessible names.

  Options:
    -i, --interactive   Show only actionable elements with ref numbers (flat list)
    --diff              Show only changes since last snapshot
    --tokens=N          Approximate token limit

  After -i, use ref numbers everywhere: click 3, type 5 \"hello\", screenshot --ref=7

  Examples:
    sidekar browser ax-tree -i
    sidekar browser ax-tree -i --diff
    sidekar browser ax-tree --tokens=2000"
        }
        "dom" => {
            "\
sidekar browser dom [selector] [--tokens=N]

  Compact DOM tree with scripts, styles, SVGs stripped.
  Traverses open shadow roots. Scope with CSS selector.

  Examples:
    sidekar browser dom
    sidekar browser dom \"main\" --tokens=3000
    sidekar browser dom \"#app\""
        }
        "screenshot" => {
            "\
sidekar browser screenshot [options]

  Capture a screenshot of the page or a specific element.

  Options:
    --ref=N            Crop to ref number (from ax-tree -i, observe, text)
    --selector=SEL     Crop to CSS selector
    --full             Capture entire scrollable page
    --annotate         Overlay numbered labels on interactive elements
    --output=PATH      Save to specific file path
    --format=FMT       png or jpeg (default: jpeg)
    --quality=N        JPEG quality 1-100
    --scale=N          Scale factor (default: fit 800px width)
    --pad=N            Padding around crop in pixels (default: 48)

  Examples:
    sidekar browser screenshot
    sidekar browser screenshot --ref=3
    sidekar browser screenshot --annotate
    sidekar browser screenshot --selector=\".modal\" --format=png
    sidekar browser screenshot --full --output=/tmp/page.png"
        }
        "press" => {
            "\
sidekar browser press <key>

  Press a key or key combination.

  Common keys: Enter, Tab, Escape, Backspace, ArrowUp, ArrowDown, Space
  Modifiers: Ctrl+A, Meta+C, Meta+V, Shift+Enter, Alt+Tab
  Mac note: Use Meta (not Ctrl) for app shortcuts. Meta+Alt+2 for Heading 2.

  Examples:
    sidekar browser press Enter
    sidekar browser press Ctrl+A
    sidekar browser press Meta+V
    sidekar browser press Shift+Enter"
        }
        "scroll" => {
            "\
sidekar browser scroll <target> [pixels]

  Scroll the page or a specific container.

  Targets:
    up / down       Scroll page (default 400px)
    top / bottom    Scroll to page extremes
    <selector>      Scroll element into view
    <selector> up   Scroll within a container

  Examples:
    sidekar browser scroll down
    sidekar browser scroll down 800
    sidekar browser scroll top
    sidekar browser scroll \".chat-messages\" down"
        }
        "search" => {
            "\
sidekar browser search <query> [--engine=E] [--tokens=N]

  Web search via real browser. Navigates to search engine, submits query,
  extracts results with 'read'. Returns formatted results.

  Engines: google (default), bing, duckduckgo, or a custom URL (query appended)

  Examples:
    sidekar browser search \"rust async programming\"
    sidekar browser search --engine=bing \"weather forecast\""
        }
        "read-urls" => {
            "\
sidekar browser read-urls <url1> <url2> ... [--tokens=N]

  Read multiple URLs in parallel. Opens each in a new tab,
  extracts content, returns combined results, closes tabs.

  Examples:
    sidekar browser read-urls https://example.com https://example.org"
        }
        "batch" => {
            "\
sidekar browser batch '<json>'

  Execute multiple actions sequentially in one call.

  JSON format: {\"actions\": [...], \"delay\": 0}
  Each action: {\"tool\": \"<cmd>\", ...params, \"wait\": ms, \"retries\": N, \"optional\": bool}
  Smart waits: 500ms auto-added after state-changing actions.

  Example:
    sidekar browser batch '{\"actions\":[
      {\"tool\":\"click\",\"target\":\"--text Continue\",\"retries\":2},
      {\"tool\":\"wait-for-nav\"},
      {\"tool\":\"screenshot\",\"output\":\"/tmp/result.png\"}
    ]}'"
        }
        "launch" => {
            "\
sidekar browser launch [options]

  Launch a Chromium browser and create a session. Idempotent — if Chrome
  for the requested profile is already running, attaches instead of
  spawning a new process.

  Most callers don't need to invoke this directly: any session-requiring
  command (navigate, click, etc.) auto-launches the default profile on
  first use. Use `launch` explicitly only to pre-warm Chrome, pick a
  non-default browser, or open a named profile.

  Options:
    --browser=NAME   chrome, edge, brave, arc, vivaldi, chromium, canary
    --profile=NAME   Named profile for isolated browser data ('new' for auto-ID)
    --headless       No visible window (all tools still work)

  See also:
    sidekar --host browser <subcommand> ...        Drive your already-running Chrome (no launch)
    sidekar --profile <name> browser <subcommand>  Managed Chrome with a named profile

  Examples:
    sidekar browser launch
    sidekar browser launch --browser=brave --profile=testing
    sidekar browser launch --headless"
        }
        "connect" => {
            "\
sidekar browser connect

  Attach to an already-running browser debug port and create a new Sidekar session.
  Does not launch a new browser process.

  Example:
    sidekar browser connect"
        }
        "sessions" => {
            "\
sidekar browser sessions <list|show> [sessionId]

  Inspect local browser sessions used by `sidekar run`.

  Subcommands:
    list               List known browser session IDs and summaries
    show <sessionId>   Show one browser session in detail

  Examples:
    sidekar browser sessions list
    sidekar browser sessions show a1b2c3d4"
        }
        "run" => {
            "\
sidekar browser run <sessionId> [<subcommand> args...]

  Run browser subcommands against an explicit saved CDP session.

  Most callers don't need this — `sidekar browser <subcommand>` auto-launches/attaches to
  the default managed Chrome. `run` targets a specific historical session ID
  (from `browser sessions list`).

  Without inline args, reads /tmp/sidekar-command-<sessionId>.json (command file).
  Command file entries use flat subcommand names: {\"command\":\"navigate\",\"args\":[\"example.com\"]}.

  Examples:
    sidekar browser sessions list
    sidekar browser run a1b2c3d4 tabs
    sidekar browser run a1b2c3d4 navigate example.com
    sidekar browser run a1b2c3d4 click 7"
        }
        "ext" => {
            "\
sidekar browser ext <subcommand> [args...]

  Drive your normal Chrome profile via the Sidekar extension. Load unpacked `extension/`
  in Chrome, then click Login with GitHub in the extension popup.

  Equivalent to `sidekar --host browser <sub>` for CDP-overlapping subs, plus extension-only
  commands (history, context, watch, monitor, eval-page, …).

  Tab IDs: from `sidekar browser ext tabs` (Chrome extension ids). Pass `--tab <id>` globally
  or as a subcommand argument; explicit subcommand tab id wins.

  Browser:
    tabs, read, screenshot, click, type, paste, set-value, ax-tree, eval, eval-page
    navigate, new-tab, close, scroll

  History & Context: history, context
  Watchers: watch, unwatch, watchers, monitor
  Management: status, stop, dev-extract

  Examples:
    sidekar browser ext tabs
    sidekar browser ext history \"terraform vpc\"
    sidekar --tab 123456789 browser ext read
    sidekar browser ext monitor start all"
        }
        "desktop" => {
            "\
sidekar desktop <subcommand> [args...]

  Desktop automation via macOS Accessibility API + SkyLight SPI.
  Background-safe — when --app/--pid is given, input is delivered
  per-pid without stealing focus or moving the cursor.

  Subcommands:
    screenshot [--app <name>|--pid <pid>] [--output <path>]
    apps                                    List running apps
    windows   --app <name>|--pid <pid>      List windows
    find      --app <name>|--pid <pid> <query>
    see       --app <name>|--pid <pid> [--annotate] [--width=N]
    set-value --app <name>|--pid <pid> --on <@eN|query> <value>
    perform-action --app <name>|--pid <pid> --on <@eN|query> --action <AXAction>
    click     --app <name>|--pid <pid> <query>
    press     [--app <name>|--pid <pid>] <key|combo>
    type      [--app <name>|--pid <pid>] [--profile human|linear] [--wpm N] [--delay MS] <text>
    paste     [--app <name>|--pid <pid>] <text>
    scroll    [--app <name>|--pid <pid>] <up|down|left|right> [amount] [page|line]
    launch    <app>
    activate  --app <name>|--pid <pid>
    quit      --app <name>|--pid <pid>
    trust                                   Check macOS permissions
    check-bg                                Verify SkyLight SPI availability
    clipboard <read|write> [text]
    menu      [list] [--app <name>|--pid <pid>]     List menu items
    menu      click --app <name> --path \"File > New\"
    dialog    list|click|input|dismiss [--app|--pid]
    window    list|focus|close|move|resize|set-bounds [--app|--pid] [--index N]
    space     list|switch --to <1-9>|move-window [--app|--pid] [--to N]
    drag      [--app|--pid] --from x,y --to x,y [--steps N]
    menubar   list|click <title>
    monitor   <start|stop|stats|log|watch>

  Background input (SkyLight SPI, macOS 14+):
    With --app/--pid, press/type/click/scroll deliver events directly
    to the target process via SLEventPostToPid — no cursor warp, no
    focus steal. Run 'sidekar desktop check-bg' to verify availability.

  Examples:
    sidekar desktop apps
    sidekar desktop screenshot --app Safari
    sidekar desktop click --app Finder \"New Folder\"
    sidekar desktop type --app Chrome \"hello world\"
    sidekar desktop type --app Chrome --profile human --wpm 120 \"hello world\"
    sidekar desktop see --app Safari --annotate
    sidekar desktop menu click --app Finder --path \"File > New Folder\"
    sidekar desktop press --app Chrome cmd+l
    sidekar desktop scroll --app Chrome down 5 page
    sidekar desktop check-bg"
        }
        "tabs" => "sidekar browser tabs\n\n  List all tabs owned by this session.",
        "tab" => "sidekar browser tab <id>\n\n  Switch to a tab by ID (from 'tabs' output).",
        "new-tab" => "sidekar browser new-tab [url]\n\n  Open a new tab, optionally navigating to URL.",
        "close" => {
            "sidekar browser close\n\n  Close the current tab. If tabs remain, select the next one explicitly with 'sidekar browser tab <id>'."
        }
        "back" => "sidekar browser back\n\n  Go back in browser history.",
        "forward" => "sidekar browser forward\n\n  Go forward in browser history.",
        "reload" => "sidekar browser reload\n\n  Reload the current page.",
        "observe" => {
            "sidekar browser observe\n\n  Show interactive elements formatted as ready-to-use commands.\n  Generates ref map. Like 'ax-tree -i' but with command suggestions."
        }
        "find" => {
            "\
sidekar browser find <query>
sidekar browser find --role <role> [name]
sidekar browser find --text <visible text>
sidekar browser find --label <label text>
sidekar browser find --testid <data-testid>

  Find elements by fuzzy query or structured semantic locators.

  Strategies:
    <query>        Fuzzy match against element role, name, and value
    --role         Exact ARIA role match (button, link, textbox, etc.)
    --text         Find by visible text content (case-insensitive)
    --label        Find by <label> or aria-label association
    --testid       Find by data-testid attribute (exact match)

  Examples:
    sidekar browser find \"submit button\"
    sidekar browser find --role button Submit
    sidekar browser find --text \"Sign in\"
    sidekar browser find --label Email
    sidekar browser find --testid login-form"
        }
        "resolve" => {
            "sidekar browser resolve <selector>\n\n  Get link/form target URL without clicking.\n  Returns href, action, formAction, src, onclick, target attributes.\n\n  Example: sidekar browser resolve 3"
        }
        _ => return None,
    })
}
