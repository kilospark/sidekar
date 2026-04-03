use std::fmt::Write;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandGroup {
    Browser,
    Page,
    Interact,
    Data,
    Desktop,
    Agent,
    Jobs,
    Account,
    System,
}

impl CommandGroup {
    fn title(self) -> &'static str {
        match self {
            Self::Browser => "Browser",
            Self::Page => "Page",
            Self::Interact => "Interact",
            Self::Data => "Data",
            Self::Desktop => "Desktop",
            Self::Agent => "Agent",
            Self::Jobs => "Jobs",
            Self::Account => "Account",
            Self::System => "System",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CommandSpec {
    pub name: &'static str,
    pub usage: &'static str,
    pub summary: &'static str,
    pub group: CommandGroup,
    pub aliases: &'static [&'static str],
    pub requires_session: bool,
    pub auto_launch_browser: bool,
    pub ext_routable: bool,
}

const REMOVED_COMMANDS: &[(&str, &str)] = &[
    ("who", "bus who"),
    ("bus-send", "bus send"),
    ("bus_send", "bus send"),
    ("bus-done", "bus done"),
    ("bus_done", "bus done"),
    ("cron-create", "cron create"),
    ("cron_create", "cron create"),
    ("cron-list", "cron list"),
    ("cron_list", "cron list"),
    ("cron-delete", "cron delete"),
    ("cron_delete", "cron delete"),
    ("desktop-screenshot", "desktop screenshot"),
    ("desktop_screenshot", "desktop screenshot"),
    ("desktop-apps", "desktop apps"),
    ("desktop_apps", "desktop apps"),
    ("desktop-windows", "desktop windows"),
    ("desktop_windows", "desktop windows"),
    ("desktop-find", "desktop find"),
    ("desktop_find", "desktop find"),
    ("desktop-click", "desktop click"),
    ("desktop_click", "desktop click"),
    ("desktop-press", "desktop press"),
    ("desktop_press", "desktop press"),
    ("desktop-type", "desktop type"),
    ("desktop_type", "desktop type"),
    ("desktop-paste", "desktop paste"),
    ("desktop_paste", "desktop paste"),
    ("desktop-launch", "desktop launch"),
    ("desktop_launch", "desktop launch"),
    ("desktop-activate", "desktop activate"),
    ("desktop_activate", "desktop activate"),
    ("desktop-quit", "desktop quit"),
    ("desktop_quit", "desktop quit"),
    ("axtree", "ax-tree"),
    ("ax_tree", "ax-tree"),
    ("newtab", "new-tab"),
    ("new_tab", "new-tab"),
    ("readurls", "read-urls"),
    ("read_urls", "read-urls"),
    ("inserttext", "insert-text"),
    ("insert_text", "insert-text"),
    ("waitfor", "wait-for"),
    ("wait_for", "wait-for"),
    ("waitfornav", "wait-for-nav"),
    ("wait_for_nav", "wait-for-nav"),
    ("sw", "service-workers"),
    ("service_workers", "service-workers"),
];

const COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec {
        name: "launch",
        usage: "[--headless]",
        summary: "Launch Chrome and start a session",
        group: CommandGroup::Browser,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "connect",
        usage: "",
        summary: "Attach to already-running Chrome (no launch)",
        group: CommandGroup::Browser,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "navigate",
        usage: "<url>",
        summary: "Navigate to URL",
        group: CommandGroup::Browser,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: true,
    },
    CommandSpec {
        name: "back",
        usage: "",
        summary: "Go back in history",
        group: CommandGroup::Browser,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "forward",
        usage: "",
        summary: "Go forward in history",
        group: CommandGroup::Browser,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "reload",
        usage: "",
        summary: "Reload the current page",
        group: CommandGroup::Browser,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "tabs",
        usage: "",
        summary: "List tabs owned by this session",
        group: CommandGroup::Browser,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: false,
        ext_routable: true,
    },
    CommandSpec {
        name: "tab",
        usage: "<id>",
        summary: "Switch to a session-owned tab",
        group: CommandGroup::Browser,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "new-tab",
        usage: "[url]",
        summary: "Open a new tab in this session",
        group: CommandGroup::Browser,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: true,
    },
    CommandSpec {
        name: "close",
        usage: "",
        summary: "Close current tab",
        group: CommandGroup::Browser,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: false,
        ext_routable: true,
    },
    CommandSpec {
        name: "activate",
        usage: "",
        summary: "Bring browser window to front (macOS)",
        group: CommandGroup::Browser,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "minimize",
        usage: "",
        summary: "Minimize browser window (macOS)",
        group: CommandGroup::Browser,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "kill",
        usage: "",
        summary: "Kill custom profile browser session",
        group: CommandGroup::Browser,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "frames",
        usage: "",
        summary: "List frames/iframes",
        group: CommandGroup::Browser,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "frame",
        usage: "<id|sel>",
        summary: "Switch frame (frame main to reset)",
        group: CommandGroup::Browser,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "read",
        usage: "[selector]",
        summary: "Reader-mode text extraction",
        group: CommandGroup::Page,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: true,
    },
    CommandSpec {
        name: "text",
        usage: "[selector]",
        summary: "Full page text with interactive refs",
        group: CommandGroup::Page,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "dom",
        usage: "[selector]",
        summary: "Get compact DOM (--tokens=N to limit output)",
        group: CommandGroup::Page,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "ax-tree",
        usage: "[selector]",
        summary: "Get accessibility tree",
        group: CommandGroup::Page,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: true,
    },
    CommandSpec {
        name: "observe",
        usage: "",
        summary: "Show interactive elements as ready-to-use commands",
        group: CommandGroup::Page,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "find",
        usage: "<query>",
        summary: "Find element by description",
        group: CommandGroup::Page,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "resolve",
        usage: "<selector>",
        summary: "Get link target URL without clicking",
        group: CommandGroup::Page,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "screenshot",
        usage: "[--full]",
        summary: "Capture screenshot (--full for entire page)",
        group: CommandGroup::Page,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: true,
    },
    CommandSpec {
        name: "pdf",
        usage: "[path]",
        summary: "Save page as PDF",
        group: CommandGroup::Page,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "search",
        usage: "<query>",
        summary: "Search the web in-browser and extract results",
        group: CommandGroup::Page,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "read-urls",
        usage: "<url1> <url2> ...",
        summary: "Read multiple URLs in parallel",
        group: CommandGroup::Page,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "grid",
        usage: "[spec]",
        summary: "Overlay coordinate grid (8x6, 50, off)",
        group: CommandGroup::Page,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "batch",
        usage: "'<json>'",
        summary: "Execute multiple actions sequentially",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "click",
        usage: "<sel|x,y|--text>",
        summary: "Click element, coordinates, or text match",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: true,
    },
    CommandSpec {
        name: "hover",
        usage: "<sel|x,y|--text>",
        summary: "Hover",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "focus",
        usage: "<selector>",
        summary: "Focus an element without clicking",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "clear",
        usage: "<selector>",
        summary: "Clear an input or contenteditable",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "type",
        usage: "<sel> <text>",
        summary: "Type text into element",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: true,
    },
    CommandSpec {
        name: "fill",
        usage: "<sel1> <val1> [sel2] [val2] ...",
        summary: "Fill multiple fields",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "keyboard",
        usage: "<text>",
        summary: "Type at current caret position",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "paste",
        usage: "<text>",
        summary: "Paste text via ClipboardEvent",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "clipboard",
        usage: "--html <html> [--text <text>]",
        summary: "Paste rich HTML",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "insert-text",
        usage: "<text>",
        summary: "Insert text at cursor via CDP Input.insertText",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "select",
        usage: "<sel> <val>",
        summary: "Select option(s) from a <select>",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "upload",
        usage: "<sel> <file>",
        summary: "Upload file(s) to a file input",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "drag",
        usage: "<from> <to>",
        summary: "Drag from one element to another",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "dialog",
        usage: "<accept|dismiss> [text]",
        summary: "Handle next dialog",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "wait-for",
        usage: "<sel> [ms]",
        summary: "Wait for element to appear",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "wait-for-nav",
        usage: "[ms]",
        summary: "Wait for navigation/readystate",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "press",
        usage: "<key>",
        summary: "Press key or combo (Enter, Ctrl+A, Meta+C)",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "scroll",
        usage: "<...>",
        summary: "Scroll page or element",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: true,
    },
    CommandSpec {
        name: "eval",
        usage: "<js>",
        summary: "Evaluate JavaScript",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: true,
    },
    CommandSpec {
        name: "media",
        usage: "<dark|light|...>",
        summary: "Emulate media features (dark mode, print, etc)",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "animations",
        usage: "<pause|resume>",
        summary: "Pause/resume page animations",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "zoom",
        usage: "<in|out|N>",
        summary: "Zoom page (25-200%, preserves layout)",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "lock",
        usage: "[seconds]",
        summary: "Lock active tab for exclusive access",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "unlock",
        usage: "",
        summary: "Release tab lock",
        group: CommandGroup::Interact,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "cookies",
        usage: "...",
        summary: "Manage cookies",
        group: CommandGroup::Data,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "console",
        usage: "...",
        summary: "Show/listen for console logs",
        group: CommandGroup::Data,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "network",
        usage: "...",
        summary: "Capture/show network requests",
        group: CommandGroup::Data,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "block",
        usage: "...",
        summary: "Configure request blocking",
        group: CommandGroup::Data,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "viewport",
        usage: "...",
        summary: "Set viewport preset or dimensions",
        group: CommandGroup::Data,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "download",
        usage: "...",
        summary: "Configure/list downloads",
        group: CommandGroup::Data,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "storage",
        usage: "<get|set|remove|clear>",
        summary: "Manage localStorage/sessionStorage",
        group: CommandGroup::Data,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "service-workers",
        usage: "<list|unregister|update>",
        summary: "Manage service workers",
        group: CommandGroup::Data,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "security",
        usage: "<ignore-certs|strict>",
        summary: "Control certificate validation",
        group: CommandGroup::Data,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "desktop",
        usage: "<subcommand> [args]",
        summary: "Desktop automation subcommands",
        group: CommandGroup::Desktop,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "bus",
        usage: "<who|requests|replies|show|send|done> [args]",
        summary: "Agent bus subcommands",
        group: CommandGroup::Agent,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "memory",
        usage: "<write|search|context|observe|sessions|compact|patterns|rate|detail|history> ...",
        summary: "Local agent memory on SQLite",
        group: CommandGroup::Agent,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "tasks",
        usage: "<add|list|done|reopen|delete|show|depend|undepend|deps> ...",
        summary: "Local task list with SQLite-backed dependencies",
        group: CommandGroup::Agent,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "agent-sessions",
        usage: "[show|rename|note] [args] [--limit=N] [--active] [--project=<name>|--all-projects]",
        summary: "Inspect local Sidekar agent session history",
        group: CommandGroup::Agent,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "repo",
        usage: "<pack|tree|changes|actions> [args]",
        summary: "Pack repos, summarize changes, and run project actions",
        group: CommandGroup::Agent,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "compact",
        usage: "<classify|filter|run> ...",
        summary: "Compact noisy command output for agent use",
        group: CommandGroup::Agent,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "monitor",
        usage: "<start|stop|status>",
        summary: "Watch tabs for background changes",
        group: CommandGroup::Agent,
        aliases: &[],
        requires_session: true,
        auto_launch_browser: true,
        ext_routable: false,
    },
    CommandSpec {
        name: "cron",
        usage: "<create|list|show|delete> [args]",
        summary: "Scheduled job subcommands",
        group: CommandGroup::Jobs,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "loop",
        usage: "<interval> <prompt_or_command>",
        summary: "Run a prompt on a recurring interval (e.g. loop 5m \"check status\")",
        group: CommandGroup::Jobs,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "device",
        usage: "<login|logout|list>",
        summary: "Device authentication and management",
        group: CommandGroup::Account,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "session",
        usage: "<list>",
        summary: "List active sessions for your account",
        group: CommandGroup::Account,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "totp",
        usage: "<subcommand>",
        summary: "Manage stored TOTP secrets",
        group: CommandGroup::Account,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "pack",
        usage: "[path|-] [--from=json|yaml|csv]",
        summary: "Pack JSON, YAML, or CSV into a compact text format",
        group: CommandGroup::Data,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "unpack",
        usage: "[path|-] [--to=json|yaml|csv]",
        summary: "Restore packed text to JSON, YAML, or CSV",
        group: CommandGroup::Data,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "kv",
        usage: "<subcommand>",
        summary: "Manage encrypted key-value storage",
        group: CommandGroup::Account,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "run",
        usage: "<sid>",
        summary: "Run command(s) from /tmp/sidekar-command-<sid>.json",
        group: CommandGroup::System,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "daemon",
        usage: "[run|stop|restart|status]",
        summary: "Manage the Sidekar background daemon",
        group: CommandGroup::System,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "config",
        usage: "list|get|set|reset",
        summary: "View or change settings",
        group: CommandGroup::System,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "feedback",
        usage: "<rating> [comment]",
        summary: "Send a rating and optional comment",
        group: CommandGroup::System,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "event",
        usage: "<list|clear> [--level=error|debug|info] [N]",
        summary: "View or clear the local event log",
        group: CommandGroup::System,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "update",
        usage: "",
        summary: "Check for updates and self-update",
        group: CommandGroup::System,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "install",
        usage: "",
        summary: "Install skill file for detected agents",
        group: CommandGroup::System,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "uninstall",
        usage: "",
        summary: "Remove sidekar data and skill files",
        group: CommandGroup::System,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "skill",
        usage: "",
        summary: "Print SKILL.md to stdout",
        group: CommandGroup::System,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
    CommandSpec {
        name: "ext",
        usage: "<sub> [args]",
        summary: "Control the browser via the extension (tabs, read, click, ...)",
        group: CommandGroup::System,
        aliases: &[],
        requires_session: false,
        auto_launch_browser: false,
        ext_routable: false,
    },
];

fn handler_name(public_name: &str) -> &str {
    match public_name {
        "ax-tree" => "axtree",
        "new-tab" => "newtab",
        "read-urls" => "readurls",
        "insert-text" => "inserttext",
        "wait-for" => "waitfor",
        "wait-for-nav" => "waitfornav",
        "service-workers" => "sw",
        other => other,
    }
}

fn public_command_spec(name: &str) -> Option<&'static CommandSpec> {
    COMMAND_SPECS.iter().find(|spec| spec.name == name)
}

pub fn removed_command_replacement(name: &str) -> Option<&'static str> {
    REMOVED_COMMANDS
        .iter()
        .find_map(|(removed, replacement)| (*removed == name).then_some(*replacement))
}

pub fn canonical_command_name(name: &str) -> Option<&'static str> {
    public_command_spec(name).map(|spec| spec.name)
}

pub fn is_known_command(name: &str) -> bool {
    public_command_spec(name).is_some()
}

pub fn command_handler(name: &str) -> Option<&'static str> {
    if let Some(spec) = public_command_spec(name) {
        return Some(handler_name(spec.name));
    }

    COMMAND_SPECS
        .iter()
        .find(|spec| handler_name(spec.name) == name)
        .map(|spec| handler_name(spec.name))
}

pub fn command_requires_session(name: &str) -> bool {
    public_command_spec(name)
        .map(|spec| spec.requires_session)
        .unwrap_or(false)
}

pub fn command_should_auto_launch_browser(name: &str) -> bool {
    public_command_spec(name)
        .map(|spec| spec.auto_launch_browser)
        .unwrap_or(false)
}

pub fn is_ext_routable_command(name: &str) -> bool {
    public_command_spec(name)
        .map(|spec| spec.ext_routable)
        .unwrap_or(false)
}

pub fn render_help(version: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "sidekar v{version}");
    let _ = writeln!(out);
    let _ = writeln!(out, "Usage: sidekar <command> [args]");
    let _ = writeln!(out);
    let _ = writeln!(out, "Commands:");

    let groups = [
        CommandGroup::Browser,
        CommandGroup::Page,
        CommandGroup::Interact,
        CommandGroup::Data,
        CommandGroup::Desktop,
        CommandGroup::Agent,
        CommandGroup::Jobs,
        CommandGroup::Account,
        CommandGroup::System,
    ];

    let visible_specs: Vec<&CommandSpec> = COMMAND_SPECS.iter().collect();
    let max_usage = visible_specs
        .iter()
        .map(|spec| display_name(spec).len())
        .max()
        .unwrap_or(0);

    for group in groups {
        let specs: Vec<&CommandSpec> = visible_specs
            .iter()
            .copied()
            .filter(|spec| spec.group == group)
            .collect();
        if specs.is_empty() {
            continue;
        }
        let _ = writeln!(out, "  {}:", group.title());
        for spec in specs {
            let usage = display_name(spec);
            let _ = writeln!(
                out,
                "    {:<width$}  {}",
                usage,
                spec.summary,
                width = max_usage
            );
        }
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "Global flags:");
    let _ = writeln!(
        out,
        "  --proxy             Enable MITM proxy for sidekar <agent>"
    );
    let _ = writeln!(
        out,
        "  --no-proxy          Disable MITM proxy for sidekar <agent>"
    );
    let _ = writeln!(
        out,
        "  --relay             Enable relay tunnel for sidekar <agent>"
    );
    let _ = writeln!(
        out,
        "  --no-relay          Disable relay tunnel for sidekar <agent>"
    );
    let _ = writeln!(
        out,
        "  --tab <id>          Target a specific tab (bypasses session; applies to sidekar ext)"
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Use 'sidekar help <command>' for detailed help on any command."
    );
    out
}

fn display_name(spec: &CommandSpec) -> String {
    if spec.usage.is_empty() {
        spec.name.to_string()
    } else {
        format!("{} {}", spec.name, spec.usage)
    }
}
