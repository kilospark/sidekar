pub fn get(command: &str) -> Option<&'static str> {
    Some(match command {
        "eval" => {
            "\
sidekar browser eval <javascript>

  Evaluate a JavaScript expression in the page context.
  Returns the result.

  Examples:
    sidekar browser eval \"document.title\"
    sidekar browser eval \"document.querySelectorAll('a').length\"
    sidekar browser eval \"document.querySelector('#btn').click()\""
        }
        "cookies" => {
            "\
sidekar browser cookies [action] [name] [value] [domain]

  Actions: get (default), set, delete, clear

  Examples:
    sidekar browser cookies
    sidekar browser cookies set session abc123
    sidekar browser cookies delete tracking
    sidekar browser cookies clear"
        }
        "console" => {
            "\
sidekar browser console [action]

  Actions:
    show (default)   Display current console messages
    listen           Stream console events (long-running)

  Examples:
    sidekar browser console
    sidekar browser console show
    sidekar browser console listen"
        }
        "network" => {
            "\
sidekar browser network [action] [args]

  CDP-sourced actions (attach debugger, shows infobar):
    capture [secs] [filter]   Record requests with headers/timing (default 10s)
    show [filter]             Re-display last capture
    har [output_path]         Export last capture as HAR 1.2

  Passive actions (extension page-world patch, no infobar):
    passive log [-n N]        Show buffered fetch/XHR/EventSource events
    passive tail [-n N]       Same as log (alias)
    passive stats             Buffer depth, totals, cap
    passive clear             Empty the passive buffer
    passive emit-off          Mute capture on already-injected documents
    passive emit-on            Resume capture (inject still runs on new docs)
    passive ... [--profile P] Target a specific extension profile

  SSE-focused views (filter + reassemble from the passive ring):
    sse streams               List captured SSE streams (active + done)
    sse log [url-substring]   Reassembled body of a stream (joined chunks)
    sse tail [url] [-n N]     Last N chunks of a stream

  Passive capture requires the Sidekar extension to be connected (see
  `sidekar browser ext status`). It sees requests as the page sees them and cannot
  modify them; use `network capture` when you need CDP-level completeness.

  Examples:
    sidekar browser network capture 15
    sidekar browser network passive log -n 20
    sidekar browser network passive stats
    sidekar browser network har /tmp/trace.har"
        }
        "block" => {
            "\
sidekar browser block <patterns...>

  Block resource types or URL patterns. Use 'off' to disable all.
  Resource types: images, css, fonts, media, scripts

  Examples:
    sidekar browser block images fonts
    sidekar browser block analytics.js
    sidekar browser block off"
        }
        "viewport" => {
            "\
sidekar browser viewport <preset|width> [height]

  Presets: mobile (375x667), iphone (390x844), ipad (820x1180),
           tablet (768x1024), desktop (1280x800)
  Or exact: sidekar browser viewport 1920 1080

  Examples:
    sidekar browser viewport mobile
    sidekar browser viewport 1440 900"
        }
        "zoom" => {
            "\
sidekar browser zoom <level>

  Zoom: in (+25%), out (-25%), reset (100%), or exact number (25-200).
  Coordinate clicks auto-adjust. Use 'zoom out' before full-page screenshots.

  Examples:
    sidekar browser zoom out
    sidekar browser zoom 50
    sidekar browser zoom reset"
        }
        "dialog" => {
            "\
sidekar browser dialog <accept|dismiss> [prompt_text]

  Set a one-shot handler for the next JavaScript dialog (alert/confirm/prompt).
  Must be called BEFORE the action that triggers the dialog.

  Examples:
    sidekar browser dialog accept
    sidekar browser dialog dismiss
    sidekar browser dialog accept \"my input text\""
        }
        "wait-for" => {
            "\
sidekar browser wait-for <selector> [timeout_ms]

  Wait for an element to appear in the DOM (default timeout: 30s).

  Examples:
    sidekar browser wait-for \".results\"
    sidekar browser wait-for \"#modal\" 5000"
        }
        "wait-for-nav" => {
            "\
sidekar browser wait-for-nav [timeout_ms]

  Wait for navigation to complete (document.readyState === 'complete').
  Default timeout: 10s.

  Example:
    sidekar browser wait-for-nav
    sidekar browser wait-for-nav 15000"
        }
        "select" => {
            "sidekar browser select <selector> <value> [value2...]\n\n  Select option(s) from a <select> element by value or label.\n\n  Example: sidekar browser select \"#country\" \"US\""
        }
        "upload" => {
            "sidekar browser upload <selector> <file> [file2...]\n\n  Upload file(s) to a file input element.\n\n  Example: sidekar browser upload \"input[type=file]\" /tmp/photo.jpg"
        }
        "drag" => {
            "sidekar browser drag <from> <to>\n\n  Drag from one element to another.\n\n  Example: sidekar browser drag \"#item-1\" \"#drop-zone\""
        }
        "paste" => {
            "sidekar browser paste <text>\n\n  Paste text via ClipboardEvent. Works with apps that intercept paste."
        }
        "clipboard" => {
            "\
sidekar browser clipboard --html <html> [--text <text>]

  Write HTML to clipboard and paste via Cmd+V.
  Works with Google Docs, Sheets, Notion — apps that ignore synthetic paste.

  Examples:
    sidekar browser clipboard --html \"<b>bold</b> text\"
    sidekar browser clipboard --html \"<h1>Title</h1>\" --text \"Title\""
        }
        "insert-text" => {
            "sidekar browser insert-text <text>\n\n  Insert text at cursor via CDP Input.insertText.\n  Faster than keyboard for large text. No formatting — use clipboard for rich text."
        }
        "hover" => {
            "sidekar browser hover <target>\n\n  Hover over an element (same targeting as click: ref, --text, selector, x,y)."
        }
        "focus" => "sidekar browser focus <selector>\n\n  Focus an element without clicking it.",
        "clear" => "sidekar browser clear <selector>\n\n  Clear an input or contenteditable element.",
        "storage" => {
            "\
sidekar browser storage <action> [key] [value] [--session]

  Actions: get, set, remove, clear
  For 'clear': target can be 'everything' (storage + cache + cookies + SW)

  Options:
    --session   Operate on sessionStorage instead of localStorage

  Examples:
    sidekar browser storage get
    sidekar browser storage set mykey myvalue
    sidekar browser storage clear everything"
        }
        "service-workers" => {
            "\
sidekar browser service-workers <action>

  Actions: list, unregister, update
  Manage service workers for the current page origin.

  Examples:
    sidekar browser service-workers list
    sidekar browser service-workers unregister"
        }
        "security" => {
            "\
sidekar browser security <action>

  Actions:
    ignore-certs   Accept self-signed/invalid certificates
    strict         Restore normal certificate validation

  Example: sidekar browser security ignore-certs"
        }
        "media" => {
            "\
sidekar browser media <features...>

  Emulate media features. Use 'reset' to restore defaults.

  Features: dark, light, print, reduce-motion, etc.

  Examples:
    sidekar browser media dark
    sidekar browser media print
    sidekar browser media reset"
        }
        "animations" => {
            "sidekar browser animations <pause|resume|slow>\n\n  pause: freeze all animations\n  resume: restore normal playback\n  slow: 10% speed"
        }
        "grid" => {
            "\
sidekar browser grid [spec]

  Overlay a coordinate grid for canvas/image targeting.

  Specs: 8x6 (cols x rows), 50 (pixel cell size), off (remove)
  Default: 10x10 grid. Take a screenshot after to see coordinates.

  Example: sidekar browser grid 8x6"
        }
        "pdf" => "sidekar browser pdf [path]\n\n  Save current page as PDF. Default: temp directory.",
        "download" => {
            "sidekar browser download [action] [path]\n\n  Actions: path (set download dir), list (show downloads)\n\n  Example: sidekar browser download path /tmp/downloads"
        }
        "frames" => "sidekar browser frames\n\n  List all frames/iframes in the page.",
        "frame" => {
            "sidekar browser frame <target>\n\n  Switch to a frame by ID, name, or CSS selector.\n  Use 'main' to switch back to the top frame.\n\n  Example: sidekar browser frame \"iframe.content\""
        }
        "lock" => {
            "sidekar browser lock [seconds]\n\n  Lock the active tab for exclusive access (default: 300s)."
        }
        "unlock" => "sidekar browser unlock\n\n  Release the tab lock.",
        "activate" => "sidekar browser activate\n\n  Bring the browser window to the front (macOS).",
        "minimize" => "sidekar browser minimize\n\n  Minimize the browser window (macOS).",
        "kill" => "sidekar browser kill\n\n  Kill the custom profile browser session.",
        "geo" => {
            "\
sidekar browser geo <lat> <lng> [accuracy]
sidekar browser geo off

  Emulate geolocation for the current page.

  Arguments:
    <lat>        Latitude (e.g. 37.7749)
    <lng>        Longitude (e.g. -122.4194)
    [accuracy]   Accuracy in meters (default: 1.0)
    off          Clear geolocation override

  Examples:
    sidekar browser geo 37.7749 -122.4194
    sidekar browser geo 51.5074 -0.1278 100
    sidekar browser geo off"
        }
        "mouse" => {
            "\
sidekar browser mouse <action> [args]

  Raw mouse primitives for fine-grained control.

  Actions:
    move <x> <y>                Move cursor to coordinates
    down [left|right|middle]    Press mouse button (default: left)
    up [left|right|middle]      Release mouse button (default: left)
    wheel <deltaY> [deltaX]     Scroll wheel (positive = down)

  Mouse position is tracked across calls (move first, then down/up/wheel).

  Examples:
    sidekar browser mouse move 100 200
    sidekar browser mouse down
    sidekar browser mouse up
    sidekar browser mouse wheel 300
    sidekar browser mouse down right"
        }
        "state" => {
            "\
sidekar browser state <save|load> [path]

  Save or restore browser state (cookies + localStorage + sessionStorage)
  as a portable JSON file.

  Subcommands:
    save [path]    Save current state to file
    load <path>    Restore state from file (navigates to original URL)

  Examples:
    sidekar browser state save /tmp/mysite.json
    sidekar browser state load /tmp/mysite.json
    sidekar browser state save"
        }
        "auth" => {
            "\
sidekar browser auth <save|login|list|delete> [args]

  Credential vault with auto-fill. Stored encrypted via KV.

  Subcommands:
    save <name> <user> <pass> [--url=<url>] [--user-selector=<sel>] [--pass-selector=<sel>]
    login <name>       Navigate + auto-detect form + fill + submit
    list               Show saved credentials
    delete <name>      Remove a credential

  Examples:
    sidekar browser auth save github myuser mypass --url=https://github.com/login
    sidekar browser auth login github
    sidekar browser auth list
    sidekar browser auth delete github"
        }
        "screencast" => {
            "\
sidekar browser screencast <start|stop|frame> [options]

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
    sidekar browser screencast start --fps=5 --quality=70
    sidekar browser screencast frame
    sidekar browser screencast stop"
        }
        _ => return None,
    })
}
