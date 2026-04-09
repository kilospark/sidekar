use crate::*;

pub fn print_command_help(command: &str) {
    if let Some(replacement) = removed_command_replacement(command) {
        println!("Command '{command}' was removed.\n\nUse: sidekar {replacement}");
        return;
    }

    let command = canonical_command_name(command).unwrap_or(command);
    let help = match command {
        "navigate" => {
            "\
sidekar navigate <url> [--no-dismiss]

  Navigate the active tab to <url>. Automatically adds https:// if no scheme.
  Auto-dismisses cookie consent banners and common popups after load.
  Returns a page brief with URL, title, visible inputs, buttons, links.

  Options:
    --no-dismiss   Skip automatic popup/banner dismissal

  Examples:
    sidekar navigate example.com
    sidekar navigate https://github.com/search?q=rust --no-dismiss"
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

  Launch a Chromium browser and create a session.

  Options:
    --browser=NAME   chrome, edge, brave, arc, vivaldi, chromium, canary
    --profile=NAME   Named profile for isolated browser data ('new' for auto-ID)
    --headless       No visible window (all tools still work)

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

        "tabs" => "sidekar tabs [--json]\n\n  List all tabs owned by this session.\n\n  Options:\n    --json    Output as JSON array",
        "tab" => "sidekar tab <id>\n\n  Switch to a tab by ID (from 'tabs' output).",
        "new-tab" => "sidekar new-tab [url]\n\n  Open a new tab, optionally navigating to URL.",
        "close" => "sidekar close\n\n  Close the current tab. If tabs remain, select the next one explicitly with 'sidekar tab <id>'.",
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

        "eval" => {
            "\
sidekar eval <javascript>

  Evaluate a JavaScript expression in the page context.
  Returns the result.

  Examples:
    sidekar eval \"document.title\"
    sidekar eval \"document.querySelectorAll('a').length\"
    sidekar eval \"document.querySelector('#btn').click()\""
        }

        "cookies" => {
            "\
sidekar cookies [action] [name] [value] [domain]

  Actions: get (default), set, delete, clear

  Examples:
    sidekar cookies
    sidekar cookies set session abc123
    sidekar cookies delete tracking
    sidekar cookies clear"
        }

        "console" => {
            "\
sidekar console [action]

  Actions:
    show (default)   Display current console messages
    listen           Stream console events (long-running)

  Examples:
    sidekar console
    sidekar console show
    sidekar console listen"
        }

        "network" => {
            "\
sidekar network [action] [duration] [filter]

  Actions:
    capture [secs] [filter]   Record requests with headers/timing (default 10s)
    show [filter]             Re-display last capture
    har [output_path]         Export last capture as HAR 1.2

  Examples:
    sidekar network capture 15
    sidekar network capture 10 api/users
    sidekar network show
    sidekar network har /tmp/trace.har"
        }

        "block" => {
            "\
sidekar block <patterns...>

  Block resource types or URL patterns. Use 'off' to disable all.
  Resource types: images, css, fonts, media, scripts

  Examples:
    sidekar block images fonts
    sidekar block analytics.js
    sidekar block off"
        }

        "viewport" => {
            "\
sidekar viewport <preset|width> [height]

  Presets: mobile (375x667), iphone (390x844), ipad (820x1180),
           tablet (768x1024), desktop (1280x800)
  Or exact: sidekar viewport 1920 1080

  Examples:
    sidekar viewport mobile
    sidekar viewport 1440 900"
        }

        "zoom" => {
            "\
sidekar zoom <level>

  Zoom: in (+25%), out (-25%), reset (100%), or exact number (25-200).
  Coordinate clicks auto-adjust. Use 'zoom out' before full-page screenshots.

  Examples:
    sidekar zoom out
    sidekar zoom 50
    sidekar zoom reset"
        }

        "dialog" => {
            "\
sidekar dialog <accept|dismiss> [prompt_text]

  Set a one-shot handler for the next JavaScript dialog (alert/confirm/prompt).
  Must be called BEFORE the action that triggers the dialog.

  Examples:
    sidekar dialog accept
    sidekar dialog dismiss
    sidekar dialog accept \"my input text\""
        }

        "wait-for" => {
            "\
sidekar wait-for <selector> [timeout_ms]

  Wait for an element to appear in the DOM (default timeout: 30s).

  Examples:
    sidekar wait-for \".results\"
    sidekar wait-for \"#modal\" 5000"
        }

        "wait-for-nav" => {
            "\
sidekar wait-for-nav [timeout_ms]

  Wait for navigation to complete (document.readyState === 'complete').
  Default timeout: 10s.

  Example:
    sidekar wait-for-nav
    sidekar wait-for-nav 15000"
        }

        "select" => {
            "sidekar select <selector> <value> [value2...]\n\n  Select option(s) from a <select> element by value or label.\n\n  Example: sidekar select \"#country\" \"US\""
        }
        "upload" => {
            "sidekar upload <selector> <file> [file2...]\n\n  Upload file(s) to a file input element.\n\n  Example: sidekar upload \"input[type=file]\" /tmp/photo.jpg"
        }
        "drag" => {
            "sidekar drag <from> <to>\n\n  Drag from one element to another.\n\n  Example: sidekar drag \"#item-1\" \"#drop-zone\""
        }
        "paste" => {
            "sidekar paste <text>\n\n  Paste text via ClipboardEvent. Works with apps that intercept paste."
        }
        "clipboard" => {
            "\
sidekar clipboard --html <html> [--text <text>]

  Write HTML to clipboard and paste via Cmd+V.
  Works with Google Docs, Sheets, Notion — apps that ignore synthetic paste.

  Examples:
    sidekar clipboard --html \"<b>bold</b> text\"
    sidekar clipboard --html \"<h1>Title</h1>\" --text \"Title\""
        }

        "insert-text" => {
            "sidekar insert-text <text>\n\n  Insert text at cursor via CDP Input.insertText.\n  Faster than keyboard for large text. No formatting — use clipboard for rich text."
        }
        "hover" => {
            "sidekar hover <target>\n\n  Hover over an element (same targeting as click: ref, --text, selector, x,y)."
        }
        "focus" => "sidekar focus <selector>\n\n  Focus an element without clicking it.",
        "clear" => "sidekar clear <selector>\n\n  Clear an input or contenteditable element.",

        "storage" => {
            "\
sidekar storage <action> [key] [value] [--session]

  Actions: get, set, remove, clear
  For 'clear': target can be 'everything' (storage + cache + cookies + SW)

  Options:
    --session   Operate on sessionStorage instead of localStorage

  Examples:
    sidekar storage get
    sidekar storage set mykey myvalue
    sidekar storage clear everything"
        }

        "service-workers" => {
            "\
sidekar service-workers <action>

  Actions: list, unregister, update
  Manage service workers for the current page origin.

  Examples:
    sidekar service-workers list
    sidekar service-workers unregister"
        }

        "security" => {
            "\
sidekar security <action>

  Actions:
    ignore-certs   Accept self-signed/invalid certificates
    strict         Restore normal certificate validation

  Example: sidekar security ignore-certs"
        }

        "media" => {
            "\
sidekar media <features...>

  Emulate media features. Use 'reset' to restore defaults.

  Features: dark, light, print, reduce-motion, etc.

  Examples:
    sidekar media dark
    sidekar media print
    sidekar media reset"
        }

        "animations" => {
            "sidekar animations <pause|resume|slow>\n\n  pause: freeze all animations\n  resume: restore normal playback\n  slow: 10% speed"
        }
        "grid" => {
            "\
sidekar grid [spec]

  Overlay a coordinate grid for canvas/image targeting.

  Specs: 8x6 (cols x rows), 50 (pixel cell size), off (remove)
  Default: 10x10 grid. Take a screenshot after to see coordinates.

  Example: sidekar grid 8x6"
        }

        "pdf" => "sidekar pdf [path]\n\n  Save current page as PDF. Default: temp directory.",
        "download" => {
            "sidekar download [action] [path]\n\n  Actions: path (set download dir), list (show downloads)\n\n  Example: sidekar download path /tmp/downloads"
        }
        "frames" => "sidekar frames\n\n  List all frames/iframes in the page.",
        "frame" => {
            "sidekar frame <target>\n\n  Switch to a frame by ID, name, or CSS selector.\n  Use 'main' to switch back to the top frame.\n\n  Example: sidekar frame \"iframe.content\""
        }
        "lock" => {
            "sidekar lock [seconds]\n\n  Lock the active tab for exclusive access (default: 300s)."
        }
        "unlock" => "sidekar unlock\n\n  Release the tab lock.",
        "activate" => "sidekar activate\n\n  Bring the browser window to the front (macOS).",
        "minimize" => "sidekar minimize\n\n  Minimize the browser window (macOS).",
        "kill" => "sidekar kill\n\n  Kill the custom profile browser session.",

        "proxy" => {
            "\
sidekar proxy <log|show|clear> [options]

  View request/response payloads captured by the proxy (--proxy flag).
  Payloads are stored in SQLite, auto-pruned after 7 days.

  Subcommands:
    log [--last=N] [--json]   List recent API calls (default: last 20)
    show <id>                 Full request/response detail with token usage
    clear                     Delete all stored payloads

  Examples:
    sidekar proxy log
    sidekar proxy log --last=5
    sidekar proxy log --json
    sidekar proxy show 42
    sidekar proxy clear"
        }

        "bus" => {
            "\
sidekar bus <who|requests|replies|show|send|done> [args...]

  Agent bus subcommands:
    who [--all] [--json]
    requests [--status=open|answered|timed-out|cancelled|all] [--limit=N]
    replies [--msg-id=<request_id>] [--limit=N]
    show <msg_id>
    send <to> <message|--file=path> [--kind=request|fyi|response] [--reply-to=<msg_id>]
    done <next> <summary> <request|--file=path> [--reply-to=<msg_id>]

  Use --file to avoid shell quoting issues — write the message to a temp file
  and pass the path instead.

  Examples:
    sidekar bus who
    sidekar bus who --all
    sidekar bus requests --status=open
    sidekar bus replies --msg-id=msg_123
    sidekar bus show msg_123
    sidekar bus send claude-2 \"Please review the PR\"
    sidekar bus send claude-2 --file=/tmp/sidekar-msg.txt
    sidekar bus done claude-2 \"Done\" --file=/tmp/sidekar-handoff.txt"
        }

        "compact" => {
            "\
sidekar compact <classify|filter|run> ...

  RTK-inspired compaction for noisy shell output in agent workflows.

  Subcommands:
    classify <command...>   Show whether Sidekar has a built-in compactor
    filter <command...>     Read raw output from stdin and compact it
    run <command> [args...] Run a command, then compact stdout/stderr

  Examples:
    sidekar compact classify git status
    cargo test 2>&1 | sidekar compact filter cargo test
    sidekar compact run cargo test"
        }

        "monitor" => {
            "\
sidekar monitor <start|stop|status> [tab_id|all]

  Watch one or more tabs for title and favicon changes, then deliver notifications
  through Sidekar's bus transport.

  Examples:
    sidekar monitor start all
    sidekar monitor start 12345 67890
    sidekar monitor status
    sidekar monitor stop"
        }

        "memory" => {
            "\
sidekar memory <write|search|context|observe|sessions|compact|patterns|rate|detail|history> ...

  Local SQLite-backed memory for Sidekar agent sessions.
  Replaces hosted memory/hook flows with in-binary storage and retrieval.

  Subcommands:
    write <type> <summary>                     Store a durable memory (project by default)
    search <query>                             Search memories in current project scope by default
    context                                    Show a scoped startup memory brief
    observe <tool> <summary>                   Append a raw observation
    sessions                                   List recent memory session summaries
    compact                                    Synthesize related project memories
    patterns                                   Promote repeated cross-project patterns
    rate <id> <helpful|wrong|outdated>         Adjust confidence on a memory
    detail <id>                                Show the full memory record
    history <id>                               Show the memory change history

  Examples:
    sidekar memory write convention \"Use Readability.js before scraping article text\"
    sidekar memory write convention \"Use Readability.js\" --scope=global
    sidekar memory search readability
    sidekar memory search readability --scope=all
    sidekar memory context
    sidekar memory compact
    sidekar memory rate 12 helpful
    sidekar memory detail 12"
        }

        "tasks" => {
            "\
sidekar tasks <add|list|done|reopen|delete|show|depend|undepend|deps> ...

  Local SQLite-backed task list with dependency edges.

  Subcommands:
    add <title> [--notes=...] [--priority=N]   Create a task (project by default)
    list [--status=open|done|all] [--ready]    List tasks in current project scope by default
    done <id>                                  Mark task done
    reopen <id>                                Mark task open again
    delete <id>                                Delete a task
    show <id>                                  Show full task details
    depend <task_id> <depends_on_id>           Add a dependency edge
    undepend <task_id> <depends_on_id>         Remove a dependency edge
    deps <id>                                  Show dependency relationships

  Examples:
    sidekar tasks add \"Ship task graph\" --priority=2
    sidekar tasks add \"Renew LLC\" --scope=global
    sidekar tasks list --ready
    sidekar tasks list --scope=all
    sidekar tasks depend 12 8
    sidekar tasks done 8
    sidekar tasks show 12"
        }

        "agent-sessions" => {
            "\
sidekar agent-sessions [show|rename|note] [args] [--limit=N] [--active] [--project=<name>|--all-projects]

  Inspect durable local Sidekar agent session metadata. Lists the current project by default.

  Commands:
    agent-sessions                           List recent sessions for the current project
    agent-sessions --all-projects            List recent sessions across all projects
    agent-sessions --active                  List only still-running sessions
    agent-sessions show <id>                 Show one session in detail
    agent-sessions rename <id> <name>        Set a friendly display name
    agent-sessions note <id> <text>          Store notes on a session
    agent-sessions note <id> --clear         Clear notes

  Examples:
    sidekar agent-sessions
    sidekar agent-sessions --active
    sidekar agent-sessions --all-projects --limit=50
    sidekar agent-sessions show pty:12345:1774750000
    sidekar agent-sessions rename pty:12345:1774750000 \"Frontend worker\"
    sidekar agent-sessions note pty:12345:1774750000 \"Owned the login fix\""
        }

        "repo" => {
            "\
 sidekar repo <pack|tree|changes|actions> [args]

  Zero-config local repo context for agents. Infers the repo root from the current
  directory, respects .gitignore and .ignore, and also reads .sidekarignore.

  Subcommands:
    pack [path]                              Pack repo files to stdout (markdown by default)
    tree [path]                              Show repo tree with estimated token counts
    changes [path]                           Summarize changed files with lightweight symbol hints
    actions [path]                           Discover likely test/lint/build/run actions
    actions run <id> [path]                  Run a discovered action with compact output

  Flags:
    --style=markdown|json|plain              Output format for pack
    --include=glob1,glob2                    Restrict to matching files
    --ignore=glob1,glob2                     Exclude additional files
    --stdin                                  Read explicit file paths from stdin
    --max-file-bytes=N                       Skip files larger than N bytes (default: 1000000)
    --diff                                   Include git worktree and staged diffs
    --logs[=N]                               Include recent git log entries (default: 10)
    --since=<ref>                            Compare changes against a git ref (changes)
    --max-files=N                            Limit reported files in changes (default: 20)
    --max-symbols=N                          Limit symbol hints per changed file (default: 20)
    --timeout=N                              Action timeout in seconds (actions run, default: 120)
    --max-output-chars=N                     Clamp action stdout/stderr (actions run, default: 12000)
    --include-output                         Include action stdout/stderr in the result

  Examples:
    sidekar repo pack
    sidekar repo tree
    sidekar repo changes
    sidekar repo changes --since=origin/main
    sidekar repo actions
    sidekar repo actions run cargo:check
    sidekar repo pack --style=json
    sidekar repo pack --include='src/**,README.md'
    rg --files src | sidekar repo pack --stdin
    sidekar repo pack --diff --logs=5"
        }

        "cron" => {
            "\
sidekar cron <create|list|show|delete> [args...]

  Scheduled job subcommands:
    create <schedule> <action_json|--prompt=TEXT|--bash=CMD> [--target=T] [--name=N] [--once]
    list
    show <job-id>
    delete <job-id>

  Action types:
    {\"tool\":\"screenshot\"}          Run a sidekar tool
    {\"batch\":[...]}                 Run a sequence of tools
    {\"prompt\":\"check status\"}      Inject a prompt into the agent
    --prompt=\"check status\"         Shorthand for prompt action
    {\"command\":\"echo hello\"}       Run a bash command
    --bash=\"echo hello\"             Shorthand for command action

  Examples:
    sidekar cron list
    sidekar cron show c727227a
    sidekar cron create \"*/5 * * * *\" '{\"tool\":\"screenshot\"}'
    sidekar cron create \"0 9 * * *\" --prompt=\"check deployment status\"
    sidekar cron create \"0 9 * * *\" --prompt=\"remind me to review PR\" --once
    sidekar cron create \"*/2 * * * *\" --bash=\"df -h\"
    sidekar cron delete 123abc"
        }

        "loop" => {
            "\
sidekar loop <interval> <prompt> [--once]

  Run a prompt on a recurring interval. Creates a cron job with a prompt
  action that gets injected into the owning agent's PTY.

  Intervals: 2m, 5m, 30m, 1h, 120s (minimum 1 minute)
  Options:
    --once   Fire once then auto-delete

  Examples:
    sidekar loop 5m \"check deployment status\"
    sidekar loop 1h \"summarize recent errors\"
    sidekar loop 10m \"remind me to review the PR\" --once"
        }

        "config" => {
            "\
sidekar config [list|get|set|reset] [key] [value]

  Manage configuration (stored in ~/.sidekar/sidekar.sqlite3).

  Commands:
    config list              Show all settings with defaults
    config get <key>         Get a single setting
    config set <key> <val>   Set a value
    config reset <key>       Revert to default

  Keys: telemetry, feedback, browser, auto_update, relay, max_tabs, cdp_timeout_secs, max_cron_jobs

  Examples:
    sidekar config list
    sidekar config set telemetry false
    sidekar config set relay off
    sidekar config set browser brave
    sidekar config reset browser"
        }

        "device" => {
            "\
sidekar device <login|logout|list>

  Manage device authentication with sidekar.dev.

  Subcommands:
    login     Authenticate this device (device auth flow)
    logout    Remove device token and clear encryption state
    list      List registered devices for your account

  Examples:
    sidekar device login
    sidekar device list
    sidekar device logout"
        }

        "session" => {
            "\
sidekar session <list>

  Manage active relay sessions.

  Subcommands:
    list      List active sessions for your account

  Examples:
    sidekar session list"
        }

        "feedback" => {
            "\
sidekar feedback <rating> [comment]

  Send a rating and optional comment to Sidekar.
  Rating must be an integer from 1 to 5.
  Disabled when `sidekar config set feedback false`.

  Examples:
    sidekar feedback 5
    sidekar feedback 3 \"Need better help output for hidden commands\""
        }

        "event" => {
            "\
sidekar event <list|clear> [--level=error|debug|info] [N]

  View or clear the local event log (SQLite). Defaults to 50 rows, all levels.

  Subcommands:
    list [--level=error|debug|info] [N]   Show recent events (newest first)
    clear [--level=error|debug|info]      Delete events (all or by level)

  Examples:
    sidekar event list
    sidekar event list --level=debug
    sidekar event list --level=error 100
    sidekar event clear
    sidekar event clear --level=debug"
        }

        "daemon" => {
            "\
sidekar daemon [start|stop|restart|status]

  Manage the background Sidekar daemon used by long-running subsystems.

  Examples:
    sidekar daemon
    sidekar daemon start
    sidekar daemon status
    sidekar daemon restart
    sidekar daemon stop"
        }

        "totp" => {
            "\
sidekar totp <add|list|get|remove> [args...]

  Store and retrieve TOTP secrets for automated login flows.
  `totp get` prints the current code only, so it is safe to pipe into other commands.

  Examples:
    sidekar totp add github alice BASE32SECRET
    sidekar totp list
    sidekar totp get github alice
    sidekar totp remove 12"
        }

        "pack" => {
            "\
sidekar pack [path|-] [--from=json|yaml|csv]

  PAKT-inspired structured packing for JSON, YAML, or CSV.
  Sidekar replaces repeated keys with a compact dictionary and emits a reversible
  text format that is easier to pass through agent context.

  Examples:
    sidekar pack data.json
    sidekar pack report.yaml
    cat rows.csv | sidekar pack --from=csv"
        }

        "unpack" => {
            "\
sidekar unpack [path|-] [--to=json|yaml|csv]

  Restore Sidekar packed text back to JSON, YAML, or CSV.
  Defaults to the original source format recorded in the packed header.

  Examples:
    sidekar unpack packed.txt
    sidekar unpack packed.txt --to=json
    cat packed.txt | sidekar unpack --to=csv"
        }

        "kv" => {
            "\
sidekar kv <subcommand> [args...]

  Encrypted key-value store with tags, versioning, and secret exec.

  Subcommands:
    set <key> <value> [--tag=a,b]   Store a value (optionally tagged)
    get <key>                       Retrieve a value
    list [--tag=TAG] [--json]       List entries (optionally filter by tag)
    delete <key>                    Delete a key and its history
    tag <add|remove> <key> <tags>   Add or remove tags on an entry
    history <key>                   Show version history
    rollback <key> <version>        Restore a previous version
    exec [--keys=K1,K2] [--tag=TAG] <cmd> [args...]
                                    Run command with secrets as env vars

  Exec injects KV values as environment variables (not argv)
  and masks secret values in output with [REDACTED].

  Examples:
    sidekar kv set STRIPE_KEY sk-abc --tag=api,prod
    sidekar kv list --tag=api
    sidekar kv tag add STRIPE_KEY billing
    sidekar kv set STRIPE_KEY sk-xyz
    sidekar kv history STRIPE_KEY
    sidekar kv rollback STRIPE_KEY 1
    sidekar kv exec --keys=STRIPE_KEY curl -H \"Bearer $STRIPE_KEY\" https://api.stripe.com
    sidekar kv exec --tag=api env"
        }

        "install" => {
            "\
sidekar install

  Install sidekar skill file for detected agents.
  Detects: Claude Code, Codex, Gemini CLI, OpenCode, Pi."
        }

        "skill" => "sidekar skill\n\n  Print the embedded SKILL.md to stdout (for agents to read).",

        "ext" => {
            "\
sidekar ext <subcommand> [args...]

  Drive your normal Chrome profile via the Sidekar extension. Load unpacked `extension/`
  in Chrome, then click Login with GitHub in the extension popup.

  Use `sidekar --tab <id> ext …` to set tab id when the subcommand omits it; an explicit
  tab id in the subcommand args wins.

  Browser:
    tabs                              List open tabs
    read [tab_id]                     Read page text
    screenshot [tab_id]               Capture visible tab
    click <selector|text:...>         Click element
    type <selector> <text>            Type into field
    paste [--html H] [--text T]       Paste content (smart fallbacks)
    set-value <selector> <text>       Set field value
    ax-tree [tab_id]                  Accessibility tree with refs
    eval <js>                         Run JS (isolated world)
    eval-page <js>                    Run JS (page world)
    navigate <url> [tab_id]           Navigate tab
    new-tab [url]                     Open new tab
    close [tab_id]                    Close tab
    scroll <up|down|top|bottom>       Scroll page

  History & Context (no CDP equivalent):
    history <query>                   Search browsing history
    context                           Active tab + windows + recent activity

  Watchers (events delivered via bus):
    watch <selector>                  Watch element, stream changes to bus
    unwatch [watchId]                 Remove watcher(s)
    watchers                          List active watchers

  Flags: --conn <id>, --profile <name>, --tab <id> (required for tab-targeted ext commands)
  Management: status, stop

  Examples:
    sidekar ext tabs
    sidekar ext history \"terraform vpc\"
    sidekar ext context
    sidekar ext watch \"span.notification-count\"
    sidekar ext paste --html \"<h1>Title</h1>\" --text \"Title\"
    sidekar ext eval-page \"window.monaco?.editor?.getEditors?.()[0]?.getValue()\""
        }

        "repl" => {
            "\
sidekar repl [-c <credential>] [-m <model>] [-p <prompt>] [-r [session_id]] [--verbose]

  Interactive LLM agent with streaming, tool calling, and session persistence.
  Credential and model may be supplied up front or selected interactively.

  Options:
    -c <credential>  Named credential (claude, codex, or-personal, claude-work, etc.)
    -m <model>       Model ID (claude-sonnet-4-5-20250514, o3, x-ai/grok-3, etc.)
    -p <prompt>      Initial prompt (skip interactive input for first turn)
    -r [session_id]  Resume a session (picker if no ID; prefix match)
    --verbose        API request/response logging and `[turn complete]` after each agent run

  Providers:
    claude     Claude (Anthropic) — OAuth device flow
    codex      Codex (OpenAI) — OAuth device flow
    or         OpenRouter — API key

  Named credentials use prefix to determine provider:
    claude-work, claude-2     → Anthropic
    codex-ci, codex-fast      → OpenAI/Codex
    or-personal, or-grok      → OpenRouter

  Environment:
    SIDEKAR_MODEL              Default model (overridden by -m)
    ANTHROPIC_API_KEY          Fallback for claude credentials
    OPENROUTER_API_KEY         Fallback for or credentials

  Subcommands:
    sidekar repl login <provider>       Store OAuth/API credentials
    sidekar repl logout [name|all]      Remove stored credentials
    sidekar repl credentials            List stored credentials
    sidekar repl models -c <credential> List available models for a provider
    sidekar repl sessions               List sessions in this directory

  Examples:
    sidekar repl login claude
    sidekar repl login or
    sidekar repl models -c claude-1
    sidekar repl sessions
    sidekar repl -c claude-1 -m claude-sonnet-4-20250514
    sidekar repl -c or -m x-ai/grok-3 -p \"explain quantum computing\"
    sidekar repl -c codex -m o3 -r
    sidekar repl -c claude-1 -r a63dcdc6
    sidekar repl credentials"
        }

        "geo" => {
            "\
sidekar geo <lat> <lng> [accuracy]
sidekar geo off

  Emulate geolocation for the current page.

  Arguments:
    <lat>        Latitude (e.g. 37.7749)
    <lng>        Longitude (e.g. -122.4194)
    [accuracy]   Accuracy in meters (default: 1.0)
    off          Clear geolocation override

  Examples:
    sidekar geo 37.7749 -122.4194
    sidekar geo 51.5074 -0.1278 100
    sidekar geo off"
        }

        "mouse" => {
            "\
sidekar mouse <action> [args]

  Raw mouse primitives for fine-grained control.

  Actions:
    move <x> <y>                Move cursor to coordinates
    down [left|right|middle]    Press mouse button (default: left)
    up [left|right|middle]      Release mouse button (default: left)
    wheel <deltaY> [deltaX]     Scroll wheel (positive = down)

  Mouse position is tracked across calls (move first, then down/up/wheel).

  Examples:
    sidekar mouse move 100 200
    sidekar mouse down
    sidekar mouse up
    sidekar mouse wheel 300
    sidekar mouse down right"
        }

        "state" => {
            "\
sidekar state <save|load> [path]

  Save or restore browser state (cookies + localStorage + sessionStorage)
  as a portable JSON file.

  Subcommands:
    save [path]    Save current state to file
    load <path>    Restore state from file (navigates to original URL)

  Examples:
    sidekar state save /tmp/mysite.json
    sidekar state load /tmp/mysite.json
    sidekar state save"
        }

        "auth" => {
            "\
sidekar auth <save|login|list|delete> [args]

  Credential vault with auto-fill. Stored encrypted via KV.

  Subcommands:
    save <name> <user> <pass> [--url=<url>] [--user-selector=<sel>] [--pass-selector=<sel>]
    login <name>       Navigate + auto-detect form + fill + submit
    list               Show saved credentials
    delete <name>      Remove a credential

  Examples:
    sidekar auth save github myuser mypass --url=https://github.com/login
    sidekar auth login github
    sidekar auth list
    sidekar auth delete github"
        }

        "screencast" => {
            "\
sidekar screencast <start|stop|frame> [options]

  Live screen capture via CDP Page.screencastFrame.

  Subcommands:
    start    Begin capturing frames to a temp JPEG file
    stop     Stop capturing
    frame    Get the latest captured frame (path + size)

  Options (start only):
    --fps=N       Target frames per second (default: 2)
    --quality=N   JPEG quality 1-100 (default: 50)
    --width=N     Max width (default: 1280)
    --height=N    Max height (default: 800)

  Examples:
    sidekar screencast start --fps=5 --quality=70
    sidekar screencast frame
    sidekar screencast stop"
        }

        "doc" => {
            "\
sidekar doc <subcommand> [args...]

  Markdown document intelligence — the prose counterpart of symbols/definition/references.

  Subcommands:
    outline <file>              Heading hierarchy with line numbers
    section <heading> [path]    Extract full text under a heading
    search <query> [path]       Keyword search across markdown sections
    map [path]                  Multi-file heading overview

  Section searches by case-insensitive substring match on heading text.
  Search matches all query terms (AND) within each line.
  Path defaults to current directory for section/search/map.

  Examples:
    sidekar doc outline README.md
    sidekar doc section Architecture README.md
    sidekar doc section \"Getting Started\"
    sidekar doc search \"browser automation\" .
    sidekar doc map docs/"
        }

        _ => {
            println!(
                "Unknown command: {command}\n\nRun 'sidekar help' for a list of all commands."
            );
            return;
        }
    };
    let text = colorize_command_help(help);
    println!("{}", crate::runtime::maybe_strip_ansi(&text));
}

/// Colorize per-command help text with ANSI codes.
fn colorize_command_help(help: &str) -> String {
    const BOLD: &str = "\x1b[1m";
    const DIM: &str = "\x1b[2m";
    const CYAN: &str = "\x1b[36m";
    const GREEN: &str = "\x1b[32m";
    const YELLOW: &str = "\x1b[33m";
    const RST: &str = "\x1b[0m";

    let mut out = String::new();
    let mut in_examples = false;

    for (i, line) in help.lines().enumerate() {
        if i == 0 {
            if let Some(rest) = line.strip_prefix("sidekar ") {
                let (cmd, args) = match rest.find(|c: char| c == ' ' || c == '<' || c == '[') {
                    Some(pos) => (&rest[..pos], &rest[pos..]),
                    None => (rest, ""),
                };
                out.push_str(&format!("{BOLD}sidekar {cmd}{RST}{DIM}{args}{RST}\n"));
            } else {
                out.push_str(&format!("{BOLD}{line}{RST}\n"));
            }
            continue;
        }

        let trimmed = line.trim();

        if trimmed.ends_with(':')
            && !trimmed.starts_with("sidekar")
            && !trimmed.starts_with("--")
            && !trimmed.starts_with('-')
            && !trimmed.contains("  ")
        {
            in_examples = trimmed == "Examples:" || trimmed == "Example:";
            out.push_str(&format!(
                "{}{YELLOW}{BOLD}{trimmed}{RST}\n",
                &line[..line.len() - trimmed.len()]
            ));
            continue;
        }

        if in_examples && trimmed.starts_with("sidekar ") {
            out.push_str(&format!(
                "{}{CYAN}{trimmed}{RST}\n",
                &line[..line.len() - trimmed.len()]
            ));
            continue;
        }

        if trimmed.starts_with("Example: sidekar ") || trimmed.starts_with("Example:  sidekar ") {
            let rest = trimmed.strip_prefix("Example:").unwrap().trim();
            out.push_str(&format!(
                "{}{YELLOW}{BOLD}Example:{RST} {CYAN}{rest}{RST}\n",
                &line[..line.len() - trimmed.len()]
            ));
            continue;
        }

        if trimmed.starts_with("--")
            || (trimmed.starts_with('-')
                && trimmed.len() > 1
                && trimmed.as_bytes()[1].is_ascii_alphabetic())
        {
            if let Some(pos) = trimmed.find("  ") {
                let flag = &trimmed[..pos];
                let desc = trimmed[pos..].trim();
                out.push_str(&format!(
                    "{}{GREEN}{flag}{RST}  {DIM}{desc}{RST}\n",
                    &line[..line.len() - trimmed.len()]
                ));
            } else {
                out.push_str(&format!(
                    "{}{GREEN}{trimmed}{RST}\n",
                    &line[..line.len() - trimmed.len()]
                ));
            }
            continue;
        }

        if !trimmed.is_empty() && !trimmed.starts_with("sidekar") && !in_examples {
            if let Some(pos) = trimmed.find("  ") {
                let left = &trimmed[..pos];
                let right = trimmed[pos..].trim();
                if !left.is_empty() && left.len() < 40 && !left.contains('.') && !right.is_empty() {
                    out.push_str(&format!(
                        "{}{CYAN}{left}{RST}  {DIM}{right}{RST}\n",
                        &line[..line.len() - trimmed.len()]
                    ));
                    continue;
                }
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    if out.ends_with('\n') {
        out.pop();
    }
    out
}

pub fn print_help() {
    let text = crate::cli::render_help(env!("CARGO_PKG_VERSION"));
    println!("{}", crate::runtime::maybe_strip_ansi(&text));
}
