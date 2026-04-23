pub const COMMANDS: &[&str] = &[
    "proxy",
    "bus",
    "compact",
    "monitor",
    "memory",
    "tasks",
    "agent-sessions",
    "repo",
    "cron",
    "loop",
    "repl",
    "doc",
];

pub fn get(command: &str) -> Option<&'static str> {
    Some(match command {
        "proxy" => {
            "\
sidekar proxy <log|show|clear> [options]

  View request/response payloads captured by the proxy (--proxy flag).
  Payloads are stored in SQLite, auto-pruned after 7 days.

  Subcommands:
    log [--last=N]            List recent API calls (default: last 20)
    show <id>                 Full request/response detail with token usage
    clear                     Delete all stored payloads

  Examples:
    sidekar proxy log
    sidekar proxy log --last=5
    sidekar --json proxy log
    sidekar proxy show 42
    sidekar proxy clear"
        }
        "bus" => {
            "\
sidekar bus <who|requests|replies|show|send|done> [args...]

  Agent bus subcommands:
    who [--all]
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
sidekar memory <write|search|context|observe|sessions|compact|hygiene|patterns|rate|detail|history> ...

  Local SQLite-backed memory for Sidekar agent sessions.
  Replaces hosted memory/hook flows with in-binary storage and retrieval.

  Subcommands:
    write <type> <summary>                     Store a durable memory (project by default)
    search <query>                             Search memories in current project scope by default
    context                                    Show a scoped startup memory brief
    observe <tool> <summary>                   Append a raw observation
    sessions                                   List recent memory session summaries
    compact                                    Synthesize related project memories
    hygiene [--project=P]                      Audit: find duplicates, stale, low-confidence, short entries
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
    sidekar memory hygiene
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
sidekar repo <pack|tree> [args]

  Zero-config local repo context for agents. Infers the repo root from the current
  directory, respects .gitignore and .ignore, and also reads .sidekarignore.

  Subcommands:
    pack [path]                              Pack repo files to stdout (markdown by default)
    tree [path]                              Show repo tree with estimated token counts

  Flags:
    --include=glob1,glob2                    Restrict to matching files
    --ignore=glob1,glob2                     Exclude additional files
    --stdin                                  Read explicit file paths from stdin
    --max-file-bytes=N                       Skip files larger than N bytes (default: 1000000)

  Examples:
    sidekar repo pack
    sidekar repo tree
    sidekar repo pack --json
    sidekar repo pack --md
    sidekar repo pack --include='src/**,README.md'
    rg --files src | sidekar repo pack --stdin"
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
        "repl" => {
            "\
sidekar repl [-c <credential>] [-m <model>] [-p <prompt>] [-r [session_id]]
             [--verbose] [--journal|--no-journal]

  Interactive LLM agent with streaming, tool calling, and session persistence.
  Credential and model may be supplied up front or selected interactively.

  Options:
    -c <credential>  Named credential (claude, codex, or-personal, grok, gem, etc.)
    -m <model>       Model ID (claude-sonnet-4-5-20250514, o3, x-ai/grok-3, etc.)
    -p <prompt>      Initial prompt (skip interactive input for first turn)
    -r [session_id]  Resume a session (picker if no ID; prefix match)
    --verbose        API request/response logging and `[turn complete]` after each agent run
    --journal        Force-enable background session journaling for this REPL (overrides config).
    --no-journal     Disable background journaling for this REPL only.
                     (Default is on; change persistently with `sidekar config set journal false`,
                     or per-process via `SIDEKAR_JOURNAL=off`. Flip at runtime with `/journal off`.)

  Providers:
    claude     Claude (Anthropic) — OAuth device flow
    codex      Codex (OpenAI) — OAuth device flow
    or         OpenRouter — API key
    oc         OpenCode — API key
    grok       Grok (xAI) — API key
    gem        Gemini (Google) — API key
    oac        Generic OpenAI-compat API

  Named credentials use prefix to determine provider:
    claude-work, claude-2     → Anthropic
    codex-ci, codex-fast      → OpenAI/Codex
    or-personal, or-grok      → OpenRouter
    oc-work, opencode-pro     → OpenCode
    grok-work                 → Grok
    oac-lab, oac-local        → OpenAI-compat

  Environment:
    SIDEKAR_MODEL              Default model (overridden by -m)
    ANTHROPIC_API_KEY          Fallback for claude credentials
    OPENROUTER_API_KEY         Fallback for or credentials
    OPENCODE_API_KEY           Fallback for oc credentials
    XAI_API_KEY                Fallback for grok credentials

  Subcommands:
    sidekar repl login <provider>                         Store OAuth/API credentials
    sidekar repl login oac <name> <url> [key] Store generic OpenAI-compat credentials
    sidekar repl logout [name|all]                        Remove stored credentials
    sidekar repl credentials                              List stored credentials
    sidekar repl models -c <credential>                   List available models for a provider
    sidekar repl sessions                                 List sessions in this directory

  Examples:
    sidekar repl login claude
    sidekar repl login or
    sidekar repl login grok
    sidekar repl login oac local http://localhost:11434/v1
    sidekar repl models -c claude-1
    sidekar repl sessions
    sidekar repl -c claude-1 -m claude-sonnet-4-20250514
    sidekar repl -c or -m x-ai/grok-3 -p \"explain quantum computing\"
    sidekar repl -c grok -m grok-4
    sidekar repl -c local -m llama3.1
    sidekar repl -c codex -m o3 -r
    sidekar repl -c claude-1 -r a63dcdc6
    sidekar repl credentials"
        }
        "doc" => {
            "\
sidekar doc <subcommand> [args...]

  Markdown document intelligence.

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
        _ => return None,
    })
}
