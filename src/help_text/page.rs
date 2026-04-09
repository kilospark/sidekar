pub const COMMANDS: &[&str] = &[
    "eval",
    "cookies",
    "console",
    "network",
    "block",
    "viewport",
    "zoom",
    "dialog",
    "wait-for",
    "wait-for-nav",
    "select",
    "upload",
    "drag",
    "paste",
    "clipboard",
    "insert-text",
    "hover",
    "focus",
    "clear",
    "storage",
    "service-workers",
    "security",
    "media",
    "animations",
    "grid",
    "pdf",
    "download",
    "frames",
    "frame",
    "lock",
    "unlock",
    "activate",
    "minimize",
    "kill",
    "geo",
    "mouse",
    "state",
    "auth",
    "screencast",
];

pub fn get(command: &str) -> Option<&'static str> {
    Some(match command {
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
        _ => return None,
    })
}
