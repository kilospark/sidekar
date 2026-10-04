pub const COMMANDS: &[&str] = &[
    "config", "prompt", "device", "relay", "event", "daemon", "totp", "hotp", "duo", "pack", "unpack", "kv",
    "install", "skill", "mcp",
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

  Keys: browser, auto_update, relay, max_tabs, cdp_timeout_secs, max_cron_jobs

  Examples:
    sidekar config list
    sidekar config set relay off
    sidekar config set browser brave
    sidekar config reset browser"
        }
        "prompt" => {
            "\
sidekar prompt [list|get|set|edit|reset|diff] [key] [text|--file=path]

  View or edit the text Sidekar ships: the prompts it sends to models, and
  the `detect.*` marker lists it matches against a wrapped agent's screen.
  Defaults ship in the binary and are seeded into the `prompts` table on
  first use. An edited entry is protected: later releases refresh only the
  entries you never touched, and `prompt list` flags yours when the default
  has moved on.

  The `detect.*` keys decide when a wrapped agent counts as blocked on a
  question, which gates bus delivery. Add a marker there when an agent
  reworks its prompts, instead of waiting for a sidekar release.

  Commands:
    prompt list              Show every prompt with size and status
    prompt get <key>         Print the active text
    prompt set <key> <text>  Replace the text (also reads --file or stdin)
    prompt edit <key>        Open the text in $EDITOR
    prompt reset <key>       Restore the shipped default
    prompt diff <key>        Show how the stored text differs from the default

  Prompt keys: pty.starter, repl.system, journal.header, journal.schema,
        journal.mode.iterative, journal.mode.fresh,
        journal.summarizer.system, compaction.system,
        compaction.instructions, memory.extract.system

  Detection keys: detect.question.markers, detect.question.yes_no,
        detect.composer.markers

  Examples:
    sidekar prompt list
    sidekar prompt get pty.starter
    sidekar prompt set repl.system --file=/tmp/system.md
    sidekar prompt edit compaction.instructions
    sidekar prompt diff repl.system
    sidekar prompt reset repl.system"
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
        "relay" => {
            "\
sidekar relay <list>

  List active relay sessions for your sidekar.dev account (remote PTY viewers).

  Subcommands:
    list      List active relay sessions

  Examples:
    sidekar relay list"
        }
        "event" => {
            "\
sidekar event <list|clear> [--level=error|debug|info] [N|--limit=N]

  View or clear the local event log (SQLite). Defaults to 50 rows, all levels.

  Subcommands:
    list [--level=error|debug|info] [N|--limit=N]  Show recent events (newest first)
    clear [--level=error|debug|info]      Delete events (all or by level)

  Examples:
    sidekar event list
    sidekar event list --level=debug
    sidekar event list --level=error 100
    sidekar event list --limit=10
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
  Secrets accept the format authenticator screens show: any case, spaces, dashes,
  trailing `=` padding. Base32 alphabet only (A-Z, 2-7), 80 bits or more.

  Examples:
    sidekar totp add github alice BASE32SECRET
    sidekar totp add microsoft alice '5qgf dbjw ysyr w2qb'
    sidekar totp list
    sidekar totp get github alice
    sidekar totp remove 12"
        }
        "hotp" => {
            "\
sidekar hotp <add|get|list|show|counter|remove> [args...]

  Counter-based one-time passwords (RFC 4226), as used by Duo Mobile passcodes.
  Stored and synced beside TOTP secrets, but each code comes from a counter that
  advances on every `get`, so a code is never issued twice (an HOTP server
  refuses a code it has already accepted).

  Use `sidekar duo enroll` for a Duo account; use `hotp add` when you already
  have the raw base32 secret and its starting counter.

  `hotp counter` resyncs the counter if the server has moved ahead of you
  (symptom: codes start being rejected).

  Examples:
    sidekar hotp add acme alice JBSWY3DPEHPK3PXP --counter=0
    sidekar hotp get acme alice
    sidekar hotp counter acme alice 40
    sidekar hotp list"
        }
        "duo" => {
            "\
sidekar duo enroll  <service> <account> <activation-url>
sidekar duo approve <service> <account> [--window <secs>]

  enroll captures a Duo Mobile enrollment: it stores the HOTP secret (so
  `sidekar hotp get <service> <account>` yields passcodes) and a device key
  (so `duo approve` can answer pushes).

  THE ACTIVATION CODE IS SINGLE USE. Run enroll BEFORE you scan the QR in Duo
  Mobile — once a phone claims the activation, there is nothing left to capture
  and you need a fresh enrollment QR. Decode the enrollment QR with any QR
  reader to get the https://api-<host>.duosecurity.com/push/v2/activation/<code>
  URL, and pass that.

  approve answers a push you just triggered: it waits up to --window seconds
  (default 30) for ONE pending push and approves it. Run it right after you
  start the login that sends the push. If two pushes are pending it approves
  neither — one may be someone else signing in as you, and the device API
  cannot tell them apart. It never polls outside that window.

  Examples:
    sidekar duo enroll msft alice 'https://api-abc123.duosecurity.com/push/v2/activation/CODE'
    sidekar hotp get msft alice
    sidekar duo approve msft alice --window 60"
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
    list [--tag=TAG]                List keys only (optional tag filter); use `get` for values
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
sidekar install [config-folder]

  Install sidekar skill file for detected agents.
  Detects: Claude Code, Codex, Gemini CLI, Grok, OpenCode, Pi.
  Honors CLAUDE_CONFIG_DIR, CODEX_HOME, GROK_HOME, etc. when set.

  config-folder  Alternate agent config root (optional).
                 Examples: claude-work → ~/.claude-work/
                           .claude-work, ~/profiles/work"
        }
        "skill" => "sidekar skill\n\n  Print the embedded SKILL.md to stdout (for agents to read).",
        "mcp" => {
            "\
sidekar mcp [install|status]

  Run a Model Context Protocol server over stdio, exposing Sidekar to apps that
  cannot run a shell command and read SKILL.md: Claude Desktop, Cowork, and
  Codex. Agents that can run shell commands should use the skill instead.

  It exposes one generic `sidekar` tool (any CLI command) plus `bus_inbox`, and
  registers on the agent bus so it can send and receive inter-agent messages.

  Subcommands:
    (none)     Serve on stdio. Apps launch this for you; you rarely run it.
    install    Register the server with Codex (codex mcp add) and Claude Desktop.
    status     Print how to add the server to an app by hand.

  Examples:
    sidekar mcp install
    sidekar mcp status"
        }
        _ => return None,
    })
}
