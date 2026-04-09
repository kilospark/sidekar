pub const COMMANDS: &[&str] = &[
    "config", "device", "session", "feedback", "event", "daemon", "totp", "pack", "unpack", "kv",
    "install", "skill", "ext",
];

pub fn get(command: &str) -> Option<&'static str> {
    Some(match command {
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

  Use `sidekar --tab <id> ext ...` to set tab id when the subcommand omits it; an explicit
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
        _ => return None,
    })
}
