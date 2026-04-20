pub const COMMANDS: &[&str] = &[
    "navigate",
    "click",
    "type",
    "keyboard",
    "fill",
    "read",
    "text",
    "ax-tree",
    "dom",
    "screenshot",
    "press",
    "scroll",
    "search",
    "read-urls",
    "batch",
    "launch",
    "connect",
    "browser-sessions",
    "run",
    "desktop",
    "tabs",
    "tab",
    "new-tab",
    "close",
    "back",
    "forward",
    "reload",
    "observe",
    "find",
    "resolve",
];

pub fn get(command: &str) -> Option<&'static str> {
    Some(match command {
        "navigate" => {
            "\
sidekar navigate <url> [--no-dismiss]

  Navigate the active tab to <url>. Automatically adds https:// if no scheme.
  Auto-dismisses cookie consent banners and common popups after load.
  Returns a page brief with URL, title, visible inputs, buttons, links.

  On first use, auto-launches managed Chrome with the 'default' profile.
  Pass --profile <name> to use a different managed profile, or --host to
  drive your already-running Chrome via the sidekar extension.

  Options:
    --no-dismiss   Skip automatic popup/banner dismissal

  Examples:
    sidekar navigate example.com
    sidekar navigate https://github.com/search?q=rust --no-dismiss
    sidekar --profile work navigate https://internal.app
    sidekar --host navigate https://news.example.com"
        }
        "click" => {
            "\
sidekar click <target> [--mode=double|right|human]

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
    sidekar click 3
    sidekar click --text \"Sign in\"
    sidekar click \"#submit-btn\"
    sidekar click --mode=double 5
    sidekar click 450,300"
        }
        "type" => {
            "\
sidekar type <selector> <text> [--human]

  Focus the element matching <selector> and type <text> into it.
  Clears existing content first.

  Options:
    --human   Human-like typing with variable delays and occasional typos

  Use 'keyboard' instead for rich text editors where focus resets cursor.

  Examples:
    sidekar type \"#search\" \"rust async\"
    sidekar type 5 \"hello world\"
    sidekar type --human \"#email\" \"user@example.com\""
        }
        "keyboard" => {
            "\
sidekar keyboard <text>

  Type text at the current caret position without focusing a new element.
  Essential for rich text editors (Slack, Google Docs, Notion) where
  'type' would reset the cursor position.

  Example:
    sidekar click \".editor\"
    sidekar keyboard \"Hello world\""
        }
        "fill" => {
            "\
sidekar fill <selector1> <value1> [selector2] [value2] ...

  Fill multiple form fields in one call. Alternating selector/value pairs.
  More efficient than multiple 'type' calls.

  Examples:
    sidekar fill \"#email\" \"user@example.com\" \"#password\" \"secret\"
    sidekar fill 3 \"Alice\" 5 \"alice@example.com\""
        }
        "read" => {
            "\
sidekar read [selector] [--tokens=N]

  Reader-mode text extraction. Strips navigation, sidebars, ads.
  Returns clean text with headings, lists, paragraphs.
  Best for articles, documentation, search results.

  Options:
    selector     CSS selector to scope extraction
    --tokens=N   Approximate token limit for output

  Examples:
    sidekar read
    sidekar read article --tokens=2000
    sidekar read \".main-content\""
        }
        "text" => {
            "\
sidekar text [selector] [--tokens=N]

  Full page text in reading order, interleaving static text with
  interactive elements (numbered refs). Like a screen reader view.
  Generates ref map as side effect.

  Best for complex pages where you need both content and interaction targets.

  Examples:
    sidekar text
    sidekar text --tokens=3000"
        }
        "ax-tree" => {
            "\
sidekar ax-tree [options] [selector]

  Accessibility tree — semantic roles and accessible names.

  Options:
    -i, --interactive   Show only actionable elements with ref numbers (flat list)
    --diff              Show only changes since last snapshot
    --tokens=N          Approximate token limit

  After -i, use ref numbers everywhere: click 3, type 5 \"hello\", screenshot --ref=7

  Examples:
    sidekar ax-tree -i
    sidekar ax-tree -i --diff
    sidekar ax-tree --tokens=2000"
        }
        "dom" => {
            "\
sidekar dom [selector] [--tokens=N]

  Compact DOM tree with scripts, styles, SVGs stripped.
  Traverses open shadow roots. Scope with CSS selector.

  Examples:
    sidekar dom
    sidekar dom \"main\" --tokens=3000
    sidekar dom \"#app\""
        }
        "screenshot" => {
            "\
sidekar screenshot [options]

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
    sidekar screenshot
    sidekar screenshot --ref=3
    sidekar screenshot --annotate
    sidekar screenshot --selector=\".modal\" --format=png
    sidekar screenshot --full --output=/tmp/page.png"
        }
        "press" => {
            "\
sidekar press <key>

  Press a key or key combination.

  Common keys: Enter, Tab, Escape, Backspace, ArrowUp, ArrowDown, Space
  Modifiers: Ctrl+A, Meta+C, Meta+V, Shift+Enter, Alt+Tab
  Mac note: Use Meta (not Ctrl) for app shortcuts. Meta+Alt+2 for Heading 2.

  Examples:
    sidekar press Enter
    sidekar press Ctrl+A
    sidekar press Meta+V
    sidekar press Shift+Enter"
        }
        "scroll" => {
            "\
sidekar scroll <target> [pixels]

  Scroll the page or a specific container.

  Targets:
    up / down       Scroll page (default 400px)
    top / bottom    Scroll to page extremes
    <selector>      Scroll element into view
    <selector> up   Scroll within a container

  Examples:
    sidekar scroll down
    sidekar scroll down 800
    sidekar scroll top
    sidekar scroll \".chat-messages\" down"
        }
        "search" => {
            "\
sidekar search <query> [--engine=E] [--tokens=N]

  Web search via real browser. Navigates to search engine, submits query,
  extracts results with 'read'. Returns formatted results.

  Engines: google (default), bing, duckduckgo, or a custom URL (query appended)

  Examples:
    sidekar search \"rust async programming\"
    sidekar search --engine=bing \"weather forecast\""
        }
        "read-urls" => {
            "\
sidekar read-urls <url1> <url2> ... [--tokens=N]

  Read multiple URLs in parallel. Opens each in a new tab,
  extracts content, returns combined results, closes tabs.

  Examples:
    sidekar read-urls https://example.com https://example.org"
        }
        "batch" => {
            "\
sidekar batch '<json>'

  Execute multiple actions sequentially in one call.

  JSON format: {\"actions\": [...], \"delay\": 0}
  Each action: {\"tool\": \"<cmd>\", ...params, \"wait\": ms, \"retries\": N, \"optional\": bool}
  Smart waits: 500ms auto-added after state-changing actions.

  Example:
    sidekar batch '{\"actions\":[
      {\"tool\":\"click\",\"target\":\"--text Continue\",\"retries\":2},
      {\"tool\":\"wait-for-nav\"},
      {\"tool\":\"screenshot\",\"output\":\"/tmp/result.png\"}
    ]}'"
        }
        "launch" => {
            "\
sidekar launch [options]

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
    sidekar --host <cmd> ...        Drive your already-running Chrome (no launch)
    sidekar --profile <name> <cmd>  Managed Chrome with a named profile

  Examples:
    sidekar launch
    sidekar launch --browser=brave --profile=testing
    sidekar launch --headless"
        }
        "connect" => {
            "\
sidekar connect

  Attach to an already-running browser debug port and create a new Sidekar session.
  Does not launch a new browser process.

  Example:
    sidekar connect"
        }
        "browser-sessions" => {
            "\
sidekar browser-sessions <list|show> [sessionId]

  Inspect local browser sessions used by `sidekar run`.

  Subcommands:
    list               List known browser session IDs and summaries
    show <sessionId>   Show one browser session in detail

  Examples:
    sidekar browser-sessions list
    sidekar browser-sessions show a1b2c3d4"
        }
        "run" => {
            "\
sidekar run <sessionId> [command args...]

  Run a command or command file against an explicit browser session.

  Most callers don't need this — `sidekar <cmd>` auto-launches/attaches to
  the default managed Chrome, and `sidekar --profile <name> <cmd>` uses a
  named profile. `run` is for cases where you want to dispatch into a
  specific historical session ID (e.g. from `browser-sessions list`).

  Without an inline command, Sidekar reads /tmp/sidekar-command-<sessionId>.json.
  With an inline command, Sidekar executes it directly against that session.

  Examples:
    sidekar browser-sessions list
    sidekar run a1b2c3d4 tabs
    sidekar run a1b2c3d4 click 7"
        }
        "desktop" => {
            "\
sidekar desktop <subcommand> [args...]

  Desktop automation via the macOS Accessibility API.

  Subcommands:
    screenshot [--app <name>|--pid <pid>] [--output <path>]
    apps
    windows --app <name>|--pid <pid>
    find --app <name>|--pid <pid> <query>
    click --app <name>|--pid <pid> <query>
    press <key|combo>
    type <text>
    paste <text>
    launch <app>
    activate --app <name>|--pid <pid>
    quit --app <name>|--pid <pid>

  Examples:
    sidekar desktop apps
    sidekar desktop screenshot --app Safari
    sidekar desktop click --app Finder \"New Folder\""
        }
        "tabs" => "sidekar tabs\n\n  List all tabs owned by this session.",
        "tab" => "sidekar tab <id>\n\n  Switch to a tab by ID (from 'tabs' output).",
        "new-tab" => "sidekar new-tab [url]\n\n  Open a new tab, optionally navigating to URL.",
        "close" => {
            "sidekar close\n\n  Close the current tab. If tabs remain, select the next one explicitly with 'sidekar tab <id>'."
        }
        "back" => "sidekar back\n\n  Go back in browser history.",
        "forward" => "sidekar forward\n\n  Go forward in browser history.",
        "reload" => "sidekar reload\n\n  Reload the current page.",
        "observe" => {
            "sidekar observe\n\n  Show interactive elements formatted as ready-to-use commands.\n  Generates ref map. Like 'ax-tree -i' but with command suggestions."
        }
        "find" => {
            "\
sidekar find <query>
sidekar find --role <role> [name]
sidekar find --text <visible text>
sidekar find --label <label text>
sidekar find --testid <data-testid>

  Find elements by fuzzy query or structured semantic locators.

  Strategies:
    <query>        Fuzzy match against element role, name, and value
    --role         Exact ARIA role match (button, link, textbox, etc.)
    --text         Find by visible text content (case-insensitive)
    --label        Find by <label> or aria-label association
    --testid       Find by data-testid attribute (exact match)

  Examples:
    sidekar find \"submit button\"
    sidekar find --role button Submit
    sidekar find --text \"Sign in\"
    sidekar find --label Email
    sidekar find --testid login-form"
        }
        "resolve" => {
            "sidekar resolve <selector>\n\n  Get link/form target URL without clicking.\n  Returns href, action, formAction, src, onclick, target attributes.\n\n  Example: sidekar resolve 3"
        }
        _ => return None,
    })
}
