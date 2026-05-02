# REPL transcript history and undo

**Workflow:** Implement on the main working tree is fine for this feature; use a git worktree only if you want a long-lived parallel experiment.

## Goal

Let users **inspect** the persisted REPL transcript (SQLite `repl_entries`, `entry_type = 'message'`) and **prune** it so the next model call reflects an earlier state — a practical **undo** without `/new`.

## Current foundation

- Transcript rows: `repl_entries` keyed by `session_id`, loaded via `session::load_history` into `Vec<ChatMessage>` in the REPL loop.
- Bulk rewrite already exists: `session::replace_history` (used by `/compact`).
- **Input line history** (`repl_input_history`) is separate — arrow keys only, not the model transcript.
- **`session_journals`** reference entry ids as text but do not FK to `repl_entries`; deleting message rows does not violate SQL constraints.

## Design

### Undo unit

- **Turn** = one `user` message plus all following non-`user` rows until the next `user` (captures assistant + tool traffic for that prompt).
- Leading non-user rows (rare) attach to an opening pseudo-turn bucket.
- **`/undo`** removes the last turn; **`/undo N`** removes the last N turns (capped at what exists).

### Prune after id

- **`/prune after <id_prefix>`** resolves a unique `repl_entries.id` prefix, then deletes every **strictly later** message row in session order (`created_at`, then `id` tie-break).
- The keep row itself remains; all newer transcript messages are removed.

### UX (slash-first)

| Command | Behavior |
|---------|----------|
| `/history` | Tail listing (cap 250); indices match **`@idx`** / **`show`** |
| `/history full` | Full transcript |
| `/history N` | Last N messages |
| `/history show idx` | Expanded body for one row |
| `/undo` | Drop last turn (default) |
| `/undo N` | Drop last N turns |
| `/prune after <prefix|@idx>` | Delete strictly newer rows |

CLI: **`sidekar repl transcript …`** mirrors list / undo / prune-after.

After any mutation: reload `history`; reset **`TurnStats`**; clear **`session_journals`** for the session via **`repl::sync_transcript_mutation_side_effects`**.

### Non-goals (v1)

- Branching via `parent_id`
- In-place editing of message bodies

## Implementation status

### Phase 1 (core)

- [x] `session::list_message_entries`, `truncate_messages_after_entry`, `undo_message_turns`, `resolve_message_entry_id_prefix`
- [x] REPL slash: `/history`, `/undo`, `/prune`
- [x] Tests for session helpers (`session/tests.rs`)
- [x] `sidekar help repl` mentions transcript slash commands

### Phase 2 (polish + CLI + docs)

- [x] Reset **`TurnStats`** after undo/prune; same after **`/compact`** when history changes (passed through `run_compact`).
- [x] **`session_journals`** (and dependent **`memory_journal_support`** / **`memory_candidates`** rows, **`memory_events_usage`** journal pointer cleared) removed on transcript mutation via **`repl::sync_transcript_mutation_side_effects`**.
- [x] CLI: **`sidekar repl transcript list|undo|prune-after`** with **`--session=`**.
- [x] Richer slash: **`/history full`**, **`/history N`**, **`/history show idx`**, **`/prune after @idx`**.
- [x] **`context/repl-agent.md`** + **`help_text`** updated.

## Files

- `src/session.rs` — listing, fetch row JSON, delete helpers
- `src/session/tests.rs` — persistence tests
- `src/repl/slash.rs` — slash handlers + `SlashResult::TranscriptUndo` / `TranscriptPruneThrough`
- `src/repl/transcript_hooks.rs` — journal cleanup hook
- `src/repl/journal/store.rs` — `delete_all_journals_for_session`
- `src/repl.rs` — `sync_transcript_mutation_side_effects` (binary-callable)
- `src/main/repl_cmd.rs` — `sidekar repl transcript …`
