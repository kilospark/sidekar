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
        "agents" => {
            "\
sidekar agents [--watch [secs]]

  Every agent on this machine, ordered by what needs you.

  States, most urgent first:
    needs input   parked on a question only a human can answer
    done          finished a turn nobody has looked at yet
    working       its screen is changing, or it is streaming output
    typing        someone is typing into it
    idle          nothing to do
    stale         still registered but has stopped reporting; check it
    dead          its process is gone; only the registration is left

  `done` means the agent settled after the last time a human typed into
  its own terminal. Typing there is what counts as having looked.

  Options:
    --watch [secs]   Redraw in place every N seconds (default 2).

  Examples:
    sidekar agents
    sidekar agents --watch
    sidekar agents --watch 5
    sidekar agents --format json"
        }
        "session" => {
            "\
sidekar session start <engine> [--cwd <dir>] [--model <m>] [--approvals ask|allow|deny] [--name <n>] [--refresh-env]
sidekar session send <name> <text|--file=path> [--wait] [--timeout <d>] [--queue|--interrupt]
sidekar session wait <name> [--turn <id>] [--timeout <d>]
sidekar session approve <name> <request_id> allow|deny [--message <why>]
sidekar session cancel|status|stop|resume <name>
sidekar session events <name> [--since <seq>] [--follow]
sidekar session list

  Run an agent in the background and drive it with structured turns: a message
  in, a result out. No terminal, no screen, no keystrokes. The engine speaks its
  own protocol (Claude: stream-json), and sidekar serves normalized events on a
  socket at ~/.sidekar/s/<name>/sock. Engines: claude.

  send --wait prints the turn's result. Exit status:
    0 finished   1 timed out (the turn keeps running; `wait` resumes)
    2 session ended   3 needs approval   4 a turn is already running
    5 the turn failed (model error, interrupted), reason on stderr

  --approvals decides what happens when the agent wants to run a tool that
  changes something (read-only tools never ask):
    ask    (default) send --wait / wait exit 3 and print the request as JSON;
           answer with `approve`, then `wait` again. Unanswered after 10m: denied.
    allow  every tool call is approved.   deny  every one is refused.

  A send while a turn runs is refused (exit 4) unless --queue (run after it)
  or --interrupt (stop it, run this). `cancel` stops the running turn.

  Every session is on the bus under its name: `sidekar bus send <name> \"...\"`
  runs a turn, and the result comes back as the reply (`bus await` works).

  If the host dies, `send` exits 2 and `resume` continues the same conversation.
  `events` prints the session's event log as JSON lines; every event keeps the
  engine's original message in `raw`.

  --refresh-env: each send carries the caller's proxy variables (http_proxy,
  https_proxy, all_proxy, no_proxy in either case, NODE_EXTRA_CA_CERTS). When
  they differ from the engine's, the engine is restarted with them, continuing
  the same conversation, just before the next turn starts — never during one.
  For environments whose proxy credentials rotate; off by default.

  Examples:
    S=$(sidekar session start claude --cwd ~/src/app)
    sidekar session send \"$S\" \"Review the diff on this branch\" --wait --timeout 20m
    sidekar session send \"$S\" \"Is the null check on line 40 a real bug?\" --wait
    sidekar session stop \"$S\""
        }
        "spawn" => {
            "\
sidekar spawn <agent> [task] [--nick <name>] [--cwd <dir>] [--model <m>] [--no-yolo]
              [--pty] [--relay|--no-relay] [--proxy|--no-proxy] [--wait] [--timeout <duration>]
sidekar spawn list

  Launch another agent, wait for it to reach the bus, and print its bus name.

  claude runs as a session (`sidekar help session`): turns go in and results
  come out over its structured protocol, and a request is answered by the turn
  it starts, with no reply command for the agent to run. --nick names the
  session. Without a bus address to answer to (a plain shell, no --wait), the
  task goes in as a turn instead, and `sidekar session wait <name>` returns its
  result. Other agents, and claude given --window, --log, --relay, --proxy or
  --pty, run in their terminal UI under the PTY wrapper.

  A task is sent as a tracked request: it carries a request id and the exact
  `bus send ... --reply-to=<id>` command to answer with, and the id is printed
  on stderr for `sidekar bus await <id>`.

  --wait stays until the agent answers and prints the answer on stdout instead
  of the name (the name goes to stderr). Exit 0 answered, 2 the agent left
  without answering, 1 no answer within --timeout (default 10m). The agent keeps
  running for follow-ups; stop it when done. Run from a plain shell, spawn gives
  itself a bus address for the length of the wait.

  --timeout takes 90s, 20m, 1h or bare seconds. Without --wait it bounds how
  long to wait for the agent to register (default 30s).

  The agent runs detached with its own session, so it outlives this command and
  ignores a Ctrl-C meant for your terminal. Unattended mode is on by default —
  each CLI spells that differently and spawn picks the right flag, so never pass
  the agent's own permission flags yourself. Use --no-yolo to leave approvals on;
  a session then waits for `sidekar session approve`.

  At most 5 spawned agents run per spawner and 15 in all; spawn refuses past
  that and names the ones running. `sidekar config set max_spawned_per_agent`
  and `max_spawned` change the limits (0 means none).

  --relay opens a relay tunnel for the agent, so it can be watched from
  sidekar.dev; --no-relay keeps it closed whatever the relay setting says.
  --proxy and --no-proxy turn the API proxy on and off the same way. All four
  reach the agent's wrapper as they would with `sidekar <agent>`.

  Two agents have no unattended mode: opencode (its --auto is only on
  `opencode run`) and pi. Spawn warns and launches them anyway; they will stop
  at their first approval prompt with nobody there to answer.

  --window opens the agent in a real terminal window instead of running it
  headless, so you can watch it and type into it. It uses the same terminal app
  you are in, read from TERM_PROGRAM; --app overrides that and accepts terminal,
  iterm, ghostty, wezterm, kitty or alacritty. A windowed agent starts in your
  current directory, not the home directory a new window would otherwise open in.

  --log <path> records the session. Headless, that is the agent's raw output;
  with --window it goes through script(1), so the window still renders normally
  and the transcript is written alongside. Expect it to fill in bursts rather
  than line by line, and to be complete once the agent exits.

  Examples:
    FINDINGS=$(sidekar spawn codex \"Review the diff on this branch.\" --wait --timeout 20m)

    REVIEWER=$(sidekar spawn codex)
    ID=$(sidekar bus send \"$REVIEWER\" \"Review the diff on this branch.\" --id-only)
    FINDINGS=$(sidekar bus await \"$ID\" --timeout 20m)
    sidekar stop \"$REVIEWER\"

    sidekar spawn claude --cwd ~/src/other-repo \"Run the test suite and report failures\"
    sidekar spawn list"
        }
        "stop" => {
            "\
sidekar stop <agent-name> [--force]

  Stop an agent that `sidekar spawn` started, by bus name.

  Sends SIGTERM so the wrapper unregisters and hands back any mail still queued
  for it, rather than stranding it. An agent sidekar did not spawn is refused,
  because stopping it would kill a pane someone else is working in; --force
  overrides that.

  Examples:
    sidekar spawn list
    sidekar stop claude-/Users/you/src/app-2"
        }
        "bus" => {
            "\
sidekar bus <who|requests|replies|show|send|done|wait|await|explain|cancel|dismiss> [args...]

  Agent bus subcommands:
    who [--all]
    requests [--status=open|answered|timed-out|cancelled|all] [--limit=N]
    replies [--msg-id=<request_id>] [--limit=N]
    show <msg_id>
    send <to> <message|--file=path> [--kind=request|fyi|response] [--reply-to=<msg_id>] [--interrupt] [--id-only]
    (--id-only prints just the request id, for `bus await`.)
    (plain send defaults to request: tracked outbound that nudges until replied.
     Short closing acks (\"ok\", \"done\", \"thanks\") and --kind=fyi send an
     untracked note that ends with \"[no reply needed]\".)
    done <next> <summary> <request|--file=path> [--reply-to=<msg_id>] [--interrupt]
    wait <agent> [--until=settled|idle|needs-input|working|user-typing] [--timeout=<ms>]
    await <msg_id> [--timeout <duration>]
    (await blocks until the request is answered and prints the answer — read from
     the broker, so it arrives in this turn rather than pasted after it. Exit 0
     answered, 2 the recipient left or the request was cancelled, 1 timed out.
     Duration: 90s, 10m, 1h or bare seconds; default 10m.)
    explain <agent>
    cancel <msg_id>... | --all
    dismiss <msg_id>...

  Use --file to avoid shell quoting issues — write the message to a temp file
  and pass the path instead.

  `wait` blocks until another agent reaches a state, for ordering work that
  `send` cannot express (start a reviewer, wait for it to be ready, then
  prompt it). Defaults to --until=settled, which returns on either idle or
  needs-input: both mean the agent stopped working. Default timeout 120000ms.
  State is published by the PTY wrapper, so the target must have been started
  with `sidekar <agent>`.

  `explain` prints the evidence behind an agent's state: which detection
  rule fired and on what line, how old the reading is, whether a finished
  turn is still unseen, and whether bus delivery is currently deferred.
  Reach for it when a message will not land or `wait` keeps timing out.
  Detection markers are editable — see `sidekar prompt`, keys `detect.*`.

  `who` flags an agent that finished a turn nobody has looked at since, so
  work waiting on you is visible without opening each terminal. A finish
  counts as seen once someone types into that agent's own terminal.

  Cross-channel messages (recipient registered on another Sidekar channel than
  you, or delivered via relay) append a short note to the pasted body so the
  recipient knows which terminal or machine should run `bus send` / `bus done`.

  Your other machines: when logged in, agents on every machine on the account
  are reachable by name or nick, and `who --all` lists them under their host.
  If two machines have an agent of the same name, add the host: name@host.
  Messages between machines travel through sidekar.dev, encrypted with your
  account key like kv, and arrive within bus_sync_interval_secs (default 15;
  0 keeps the bus on this machine).

  `--interrupt` asks a receiving `sidekar repl` or known Sidekar PTY-wrapped
  agent CLI (claude, codex, cursor-agent, gemini, grok, opencode, etc.) to
  cancel its active turn before delivering the message.

  `cancel` stops any pending nudges for one or more of your own outbound
  requests. Pass explicit msg_ids or --all to close every open request
  owned by the current agent in one shot. The recipient is not notified.

  `dismiss` stops future nudges for requests addressed to you without
  delivering a response into the sender's terminal. It records a local
  \"dismissed\" reply when both agents share the same broker DB.

  Examples:
    sidekar bus who
    sidekar bus who --all
    sidekar bus requests --status=open
    sidekar bus replies --msg-id=msg_123
    sidekar bus show msg_123
    sidekar bus send claude-2 \"Please review the PR\"
    sidekar bus send claude-2@studio \"Same name on two machines: pick one\"
    sidekar bus send claude-2 --file=/tmp/sidekar-msg.txt
    sidekar bus done claude-2 \"Done\" --file=/tmp/sidekar-handoff.txt
    sidekar bus cancel msg_123 msg_456
    sidekar bus cancel --all
    sidekar bus dismiss msg_123"
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

  REPL agents: drive this only via the Sidekar tool (args [\"monitor\", …]),
  not Bash(`sidekar …`) — see embedded tool description \"Tab monitor\".

  `monitor start` blocks until Ctrl-C so watcher stays alive in that process.
  From REPL via the Sidekar tool, start returns immediately; watcher keeps running
  for REPL process lifetime. `status` only reflects monitors in same OS process.

  Examples:
    sidekar monitor start all
    sidekar monitor start 12345 67890
    sidekar monitor status
    sidekar monitor stop"
        }
        "memory" => {
            "\
sidekar memory <write|archive|search|list|delete|context|compact|hygiene|patterns|rate|detail|usage|candidates|import> ...

  Local SQLite-backed memory for Sidekar agent sessions.
  Replaces hosted memory/hook flows with in-binary storage and retrieval.

  Subcommands:
    write <type> <summary>                     Store a durable memory (project by default)
    archive [--file=P | -] [--title=T]         Archive a session summary/transcript an agent hands you
    search <query>                             Search memories in current project scope by default
    list                                       List recent memories by scope/type
    delete <id>                                Delete a memory by id
    context                                    Show a scoped startup memory brief
    compact                                    Synthesize related project memories
    hygiene [--project=P]                      Audit: find duplicates, stale, low-confidence, short entries
    patterns                                   Promote repeated cross-project patterns
    rate <id> <helpful|wrong|outdated>         Adjust confidence on a memory
    detail <id>                                Show the full memory record
    usage <id>                                 Show where a memory has been used
    candidates                                 Review journal-extracted memory candidates
    import [--source=<list>] [--dry-run]       Import memories from ~/.claude, ~/.codex, etc.

  memory write stores one distilled learning (deduped). memory archive stores a
  whole session summary verbatim (never deduped) so any agent can \"send a copy
  of this session to sidekar\" — pipe it in or pass --file (up to 1 MiB). Both
  are searchable.

  Logged in (sidekar device login), memory syncs across your devices, encrypted
  with your account key like kv and totp. A memory uploads only to the account
  it was written under; memories written while logged out upload to the next
  account you log in with. `sidekar kv sync-status` shows what is pending.

  Examples:
    sidekar memory write convention \"Use Readability.js before scraping article text\"
    sidekar memory write convention \"Use Readability.js\" --scope=global
    echo \"<session summary>\" | sidekar memory archive --title=\"login debug\" --from=muse
    sidekar memory archive --file=summary.md --tags=auth,mfa
    sidekar memory search readability
    sidekar memory search readability --scope=all
    sidekar memory context
    sidekar memory compact
    sidekar memory rate 12 helpful
    sidekar memory detail 12
    sidekar memory import --source=codex --since=14d --max-sessions=10"
        }
        "journal" => {
            "\
sidekar journal <status|list|show> [args]

  Inspect session journaling. Journals are automatic, structured,
  per-session summaries; they are a recall aid, not a replacement for
  durable `memory` entries.

  Subcommands:
    status                     Is journaling actually running on this
                               machine, what credential it uses, and what
                               it has imported. Start here when nothing
                               seems to be getting remembered.
    list [N] [--project=P]     Recent journals (default N=10, max 200).
                               --project overrides the cwd scope.
    show <id>                  Full 12-section view of one journal.

  Two write paths:
    REPL sessions journal themselves. The background polling task
    triggers every SIDEKAR_JOURNAL_IDLE_SECS seconds of idleness
    (default 90) and writes a 12-section journal.

    PTY-wrapped agents (`sidekar claude`, `codex`, `cursor-agent`,
    `gemini`, `opencode`, `copilot`) cannot be watched that way, so on
    exit sidekar runs `memory import` over that harness transcript
    instead. Those land in `memory`, not in `journal list`.

  Both need an LLM credential and both honour the same switch:
    /journal on|off, --journal / --no-journal on `sidekar repl`, or
    `sidekar config set journal true|false`. The credential comes from
    `sidekar config set credential <name>` — or is taken automatically
    when exactly one is stored.

  Examples:
    sidekar journal status
    sidekar journal list
    sidekar journal list 30
    sidekar journal list --project=sidekar
    sidekar journal show 42"
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

  Slash transcript: /history (full | tail N | show idx), /undo [N], /prune after <id_prefix|@idx>.
  Undo/prune/compact reset usage counters for /status and clear session journals whose entry pointers
  would be stale. Non-interactive: sidekar repl transcript list|undo|prune-after [--session=P].
  Options:
    -c <credential>  Stored credential name (`oauth:<name>` key): defaults like anthropic, gemini, or a nickname from `credential add`
    -m <model>       Model ID (claude-sonnet-4-5-20250514, o3, grok-build-0.1, grok-4.3, etc.)
    -p <prompt>      Initial prompt (skip interactive input for first turn)
    -r [session_id]  Resume a session (picker if no ID; prefix match)
    --verbose        API request/response logging and `[turn complete]` after each agent run
    --journal        Force-enable background session journaling for this REPL (overrides config).
    --no-journal     Disable background journaling for this REPL only.
                     (Default is on; change persistently with `sidekar config set journal false`,
                     or per-process via `SIDEKAR_JOURNAL=off`. Flip at runtime with `/journal off`.)

  Providers:
    claude           Claude (Anthropic) — OAuth device flow
    codex            Codex (OpenAI) — OAuth device flow
    openrouter       OpenRouter — API key
    opencode-zen     OpenCode Zen — API key
    opencode-go      OpenCode Go — API key
    grok             Grok (xAI) — Grok Build OAuth → cli-chat-proxy; or XAI_API_KEY on api.x.ai
    gemini           Gemini (Google) — API key
    bedrock          Amazon Bedrock — IAM / SigV4
    vertex           GCP Vertex AI (OpenAI-compat) — project + region; Bearer via `gcloud`
    openai-compat    Generic OpenAI-compat API

  Stored credential names (`oauth:<key>`):
    Choose any unique key when adding (`credential add openrouter personal` → key `personal`).
    Provider type is always taken from saved credential metadata, not from the key string.

  Environment:
    SIDEKAR_MODEL              Default model (overridden by -m)
    ANTHROPIC_API_KEY          Fallback for claude credentials
    OPENROUTER_API_KEY         Fallback for openrouter credentials
    OPENCODE_API_KEY           Fallback for opencode-zen / opencode-go credentials
    GEMINI_API_KEY / GOOGLE_API_KEY   Fallback for gemini credentials
    XAI_API_KEY                Fallback for grok credentials

  Subcommands:
    sidekar repl credential                              Credential help (providers + examples)
    sidekar repl credential add <provider> [nickname]    Store OAuth/API credentials
    sidekar repl credential add openai-compat <nick> <url> [key]  Store generic OpenAI-compat credentials
    sidekar repl logout [nickname|all]                   Remove stored credentials
    sidekar repl credentials                            List stored credentials
    sidekar repl models -c <credential>                   List available models for a provider
    sidekar repl sessions                                List sessions in this directory
    sidekar repl transcript list [--session=P] [--full] [--limit N]
    sidekar repl transcript undo [--session=P] [N]
    sidekar repl transcript prune-after [--session=P] <id_prefix|@index>

  Examples:
    sidekar repl credential add claude
    sidekar repl credential add claude work           → stored as 'work'
    sidekar repl credential add openrouter personal   → stored as 'personal'
    sidekar repl credential add grok          # Grok Build OAuth (browser)
    sidekar repl credential add grok --api-key  # console.x.ai API key instead
    sidekar repl credential add vertex prod           → stored as 'prod'
    sidekar repl credential add openai-compat local http://localhost:11434/v1
    sidekar repl models -c claude-1
    sidekar repl sessions
    sidekar repl -c claude-1 -m claude-sonnet-4-20250514
    sidekar repl -c grok -m grok-build-0.1
    sidekar repl -c grok -m grok-build        # alias → grok-build-0.1
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
