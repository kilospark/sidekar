# TODO: Session Journaling

Status: **planned, not implemented**. Design frozen (2026-04-22). Build when picked off main TODO.

## One-line summary

Background, idle-triggered LLM summarization of ongoing REPL sessions. Writes structured entries to `~/.sidekar/broker.db`. Consumed on session resume (prompt injection), in `/session` listing (one-line teaser), and via a new `/journal` slash command and `sidekar journal …` CLI.

## Why

Long REPL sessions accumulate context that doesn't survive `/new`, laptop reboots, or context-window compaction. Hermes and claw42 both show this works; neither has exactly the shape we want. See `context/coding-agent-memory-prompts-analysis.md` for the cross-repo analysis.

**What this is not**: not global/cross-project memory (that's `sidekar memory`), not context-window compaction (that's `src/agent/compaction.rs`), not a daemon-side feature. Journaling is per-session continuity that the REPL owns.

## Design decisions (settled)

### Storage

- **Single DB**: `~/.sidekar/broker.db`. No new files, no `<project>/.sidekar/` directory. User explicitly requested staying in the existing DB.
- **One new table**: `session_journals`. No schema changes to `memory_events`, `repl_entries`, or `repl_sessions`.

```sql
CREATE TABLE IF NOT EXISTS session_journals (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES repl_sessions(id),
    project TEXT NOT NULL,              -- cwd of the session, for cross-session project queries
    created_at REAL NOT NULL,
    -- Range of repl_entries this journal covers, so the next pass
    -- resumes from to_entry_rowid+1 and never re-summarizes.
    from_entry_rowid INTEGER NOT NULL,
    to_entry_rowid INTEGER NOT NULL,
    -- Structured output from the summarizer. Arrays stored as JSON
    -- TEXT; simpler than a child table for the expected volume
    -- (~10-50 entries per long session, hundreds per project/year).
    summary TEXT NOT NULL,                       -- 2-4 sentence narrative
    decisions_json TEXT NOT NULL DEFAULT '[]',   -- ["...", "..."]
    next_steps_json TEXT NOT NULL DEFAULT '[]',  -- ["...", "..."]
    open_questions_json TEXT NOT NULL DEFAULT '[]',
    files_touched_json TEXT NOT NULL DEFAULT '[]',
    -- Audit / cost tracking.
    model_used TEXT NOT NULL,
    cred_used TEXT NOT NULL,
    tokens_in INTEGER NOT NULL DEFAULT 0,
    tokens_out INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_session_journals_session
    ON session_journals(session_id, created_at);
CREATE INDEX IF NOT EXISTS idx_session_journals_project
    ON session_journals(project, created_at DESC);
```

### Location of logic

- **REPL process, not daemon.** User rejected daemon-side because the daemon doesn't know which credential/model a given REPL is using, and idle detection is trivial in the REPL (we know when we're waiting on prompt input) but indirect in the daemon.
- Per-REPL-session lifetime. When the REPL exits, its idle watcher exits. If the user closes the REPL mid-thought, that session's final arc is not journaled — accepted trade-off for v1.

### Trigger

After each `StreamEvent::Done`:
- Arm a tokio timer for `SIDEKAR_JOURNAL_IDLE_SECS` (default **90s**).
- On user keystroke: cancel timer; re-arm after next Done.
- On timer fire: spawn journaling task iff all of:
  1. Env `SIDEKAR_JOURNAL=1` set (opt-in for v1; re-evaluate default after burn-in).
  2. ≥4 new turns since the last journal entry for this session (tune with `SIDEKAR_JOURNAL_MIN_TURNS`).
  3. No journaling currently in flight for this session (per-session mutex).
  4. Cost cap: last 24h `SUM(tokens_in)` for this project across all sessions < **50,000** tokens (`SIDEKAR_JOURNAL_MAX_TOKENS_24H`).

All thresholds env-configurable; defaults in `src/repl/journal.rs`.

### Model selection

- Default: the REPL's currently active provider + model. Billed to the user's active credential.
- Override: `SIDEKAR_JOURNAL_MODEL` + `SIDEKAR_JOURNAL_CRED` to route summaries to a cheap model (e.g. `gem-personal` / `gemini-2.5-flash`) while the main REPL uses a larger model.
- If no credential available (REPL hasn't picked one), skip silently, log once under `broker.try_log_event("info", "journal", "skipped — no cred")`.

### Summarization prompt

Adopt **pi-mono's 7-section format** (see `context/coding-agent-memory-prompts-analysis.md:548-557`):

```
## Goal
## Constraints & Preferences
## Progress (Done / In Progress / Blocked)
## Key Decisions
## Next Steps
## Critical Context
## File Operations (read-files / modified-files)
```

But structured output, not free-form markdown. Prompt asks for a strict JSON object:

```json
{
  "summary": "...",
  "decisions": ["..."],
  "next_steps": ["..."],
  "open_questions": ["..."],
  "files_touched": ["..."]
}
```

Parse defensively. If JSON parse fails, fall back to storing the full response text in `summary` and leaving the arrays empty. Never lose a row to bad provider output.

### Cancellation

Reuse the existing `cancel: Arc<AtomicBool>` from the REPL. Any user keystroke during a journaling pass flips it; the summarizer's stream-discard loop checks it between deltas and bails. Cancelled runs do not write a row; they retry on the next idle.

## Consumption — how journals get used

Three places:

### (a) Session resume — system prompt injection

- On `/session` switch or `--resume <id>` cold start, query:
  ```sql
  SELECT summary FROM session_journals
   WHERE session_id = ?
   ORDER BY created_at DESC
   LIMIT 3
  ```
- Inject into the system prompt as a new section **before** the existing `AGENTS.md` content:
  ```
  Recent session journal:
  - [3h ago] Fixed OAuth state bug for Codex; Anthropic still requires it.
  - [2h ago] Added /status command; distinct from /stats.
  - [15m ago] Landed preflight version-drift guard in release scripts.
  ```
- Controlled by `SIDEKAR_JOURNAL_INJECT=1` (default on once journal feature enabled).
- Not injected on `/new` — fresh sessions don't carry prior journal context.
- Not injected on initial REPL start for a brand-new cwd — no prior session means nothing to inject.

### (b) `/session` listing

- Extend `session::SessionWithCount` with `last_journal_summary: Option<String>`.
- SQL join:
  ```sql
  LEFT JOIN (
    SELECT session_id, summary
      FROM session_journals sj1
     WHERE created_at = (
       SELECT MAX(created_at) FROM session_journals sj2
        WHERE sj2.session_id = sj1.session_id
     )
  ) last_journal ON last_journal.session_id = sessions.id
  ```
- `/session` renders: below the existing snippet+age line, if a journal exists, add `journal: <30-char teaser>` dim-italic.

### (c) New slash command `/journal`

- `/journal` — show last 3 entries for current session, pretty-printed.
- `/journal all` — show all entries for current session (paginated with `less` if long).
- `/journal now` — force an immediate journaling pass, ignoring the ≥4-turns threshold. Useful for tests and "about to close laptop" capture.

No edit/delete in v1. Raw `sqlite3 ~/.sidekar/broker.db` access is fine for the rare scrub case.

### (d) Startup brief (optional, off by default)

- `src/memory/commands.rs` already has a `startup_brief` function shown at REPL start.
- Add a `journal_brief` that shows "Last seen on this project: …" from the most recent journal entry for this cwd, across all sessions.
- Gated by `SIDEKAR_JOURNAL_BRIEF=1`. Off by default — startup chatter.

## CLI surface

```
sidekar journal list [--session <id>] [--project <path>] [--limit N]
sidekar journal show <id>
sidekar journal tail                       # follow, new entries as written
sidekar journal run --session <id>         # force a pass
```

## Implementation order (commits)

1. **Schema + storage helpers**: migration in `src/broker.rs`, insert/select functions in new `src/repl/journal_store.rs`. Unit-tested with tempfile DB.
2. **Pure formatter + parser**: `format_journal_prompt(history_slice)` and `parse_journal_response(text) -> JournalEntry`. No LLM, no tokio. Unit tests for valid JSON, malformed JSON, missing fields, huge responses.
3. **REPL idle integration**: timer in `src/repl.rs`, background task in `src/repl/journal.rs`. Cancellation reuses existing `Arc<AtomicBool>`. Wire `on_event` to arm the timer after `StreamEvent::Done`.
4. **Consumption**:
   - Resume injection in `src/repl/system_prompt.rs`.
   - `/session` teaser in `src/repl/slash.rs` + SQL change in `src/session.rs`.
   - `/journal` slash command.
5. **CLI**: `sidekar journal …` subcommand tree in `src/main/` + `src/commands/`.
6. **Docs**: update `context/repl-agent.md` with journaling section; update `context/storage-schema.md` with the new table.

Each a separate commit so review stops cleanly between steps.

## Deferred to v2+ (out of scope here)

- Embeddings / semantic search across journals.
- Daemon-side journaling (covers sessions after REPL closed).
- Auto-promotion of journal decisions into `memory_events` (reinforcement pattern — "if the same decision appears in N journals, write to memory with confidence 0.4").
- Cross-session stitching / weekly rollups.
- UI for reviewing/editing journal entries.
- Per-project redaction rules.
- Scheduled journaling on daemon timer as a belt-and-suspenders companion to REPL-side.

## Prior art references

- `context/coding-agent-memory-prompts-analysis.md`:
  - hermes MEMORY.md/USER.md frozen-snapshot pattern (lines 324-349)
  - pi-mono 7-section compaction format (lines 538-559)
  - free-code 9-section summary template at token thresholds (lines 519-536)
  - claw42 heartbeat promoting daily→core memories ~12h (row on line 33)
- `context/repl-memory-prompt-design.md`: cross-repo prompt architecture table.
- `src/agent/compaction.rs`: existing two-phase compaction — journaling should NOT touch this, they solve different problems.
- `src/memory.rs` + `src/memory/`: existing structured memory with decision/convention/constraint/preference/open-thread/artifact-pointer types. Journaling complements but does not overlap.

## Open questions to revisit before implementation

1. After burn-in, flip `SIDEKAR_JOURNAL` to default-on?
2. Should `/journal now` bypass the cost cap? (Lean: yes — user-initiated, they accept the bill.)
3. Two distinct FTS5 vtables on journal `summary` for search, or defer until someone asks?
4. What happens when a session's history is compacted between journaling passes? The `from_entry_rowid`/`to_entry_rowid` become stale pointers. Proposed: at journal-pass start, detect "history shorter than expected" and reset `last_journaled_idx` to 0; treat compaction as "journal the whole post-compact state fresh."
