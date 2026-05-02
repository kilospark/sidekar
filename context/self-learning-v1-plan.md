# Sidekar Self-Learning V1 Plan

## Goal

Turn REPL from "memory + journal assisted" into default-on self-learning system with closed loop:

1. capture useful evidence automatically
2. extract candidate learnings automatically
3. validate / supersede / reinforce automatically
4. retrieve smallest relevant set automatically
5. maintain memory quality automatically

V1 target is not full autonomy. V1 target is reliable low-noise loop with observability and bounded cost.

## Current State

Existing pieces:

- startup memory brief injection
- prior journal injection into system prompt
- idle background journaling
- journal-to-memory promotion for repeated constraints / decisions
- memory dedup + supersession support

Missing pieces:

- guaranteed final flush on exit
- automatic end-of-turn candidate extraction
- task/path-aware retrieval
- explicit outcome-based reinforcement
- contradiction review loop beyond basic near-match supersession
- observability for why a memory was created/injected/reinforced
- crash/interruption recovery
- durable unresolved-thread/task learning

## V1 Scope

V1 includes:

1. final journal flush on clean REPL exit and single-prompt exit
2. automatic candidate extraction from persisted journals
3. stronger contradiction / supersession rules for new auto memories
4. task/path-aware retrieval surface for startup and active turns
5. explicit reinforcement on successful usage and accepted outcomes
6. observability commands / fields to inspect learning lifecycle

V1 excludes:

- cross-project "dreaming" / speculative synthesis
- background repo-wide file import by default
- external daemon worker farm
- high-cost always-on LLM learning on every micro-event

## Architecture

### Loop A: Capture

Raw sources:

- user turns
- assistant final outputs
- tool calls + tool results
- journal rows
- session/project outcome markers

Storage:

- keep existing `repl_entries`
- keep existing `session_journals`
- add structured learning candidates table for extracted-but-not-yet-promoted facts

### Loop B: Extract

Trigger points:

- after journal persist
- on final exit flush
- optional future: after explicit successful turn completion

Pipeline:

1. heuristic classifier
2. structured extractor
3. optional LLM fallback for ambiguous high-signal material
4. write candidate rows with provenance

Candidate types:

- `decision`
- `constraint`
- `convention`
- `preference`
- `open-thread`
- `artifact-pointer`
- `failure-pattern`
- `workflow`

### Loop C: Validate / Promote

Rules:

- exact dedup -> reinforce existing
- high-overlap + same type -> candidate supersession
- repeated support across journals / sessions -> promote
- explicit contradiction -> supersede old row, keep lineage
- unresolved thread aging -> keep active until resolved or stale

### Loop D: Retrieve

Retrieval dimensions:

- project
- global scope
- active cwd / path prefix
- tool / provider family
- task keywords from latest user turn
- recency
- confidence
- reinforcement count

Surfaces:

- startup brief
- per-turn system prompt augmentation
- `/memory why`
- `/status`

### Loop E: Maintain

Background maintenance:

- merge duplicates
- decay weak unreinforced auto-learned rows
- compact near-identical memories
- mark stale open threads
- recalculate retrieval rank features

## Data Model Changes

### New table: `memory_candidates`

Purpose:

- hold auto-extracted facts before durable promotion

Fields:

- `id`
- `project`
- `session_id`
- `journal_id`
- `event_type`
- `scope`
- `summary`
- `summary_norm`
- `confidence`
- `status` (`new|promoted|rejected|superseded`)
- `source_kind`
- `trigger_kind`
- `evidence_json`
- `related_memory_id`
- `created_at`
- `updated_at`

### New table: `memory_events_usage`

Purpose:

- record when a memory was injected / referenced / reinforced / contradicted

Fields:

- `id`
- `memory_id`
- `session_id`
- `journal_id`
- `entry_id`
- `usage_kind` (`injected|selected|reinforced|contradicted|accepted|resolved`)
- `detail_json`
- `created_at`

### Existing table changes

`memory_events` additions:

- `origin` (`user|journal|extractor|import`)
- `path_hint`
- `task_hint`
- `last_selected_at`
- `selection_count`
- `last_contradicted_at`

## Retrieval Changes

### Startup

Replace blunt `startup_brief(limit)` with ranked retrieval:

1. project/global filter
2. match current cwd / project path hints
3. prefer active open threads + constraints + preferences
4. cap by token budget, not fixed item count

### Active Turn

Before model call:

1. derive task terms from latest user turn
2. retrieve top matching memories
3. inject compact "relevant memory" block
4. log why each item was selected

## Reinforcement Rules

Positive signals:

- journal preserved same fact again
- user did not correct memory and turn succeeded
- tests/commands succeeded after memory-guided action
- memory explicitly selected for active context

Negative signals:

- user corrects model immediately
- new memory strongly contradicts old memory
- memory repeatedly selected but never co-occurs with success

Confidence updates:

- small bounded increments on reinforcement
- bounded decrements on contradiction / staleness
- auto-learned rows cap below explicit user-authored rows until reinforced

## Observability

Need user-facing inspection:

- `/memory why`
- `/memory usage <id>`
- `/memory candidates`
- `/memory candidates promote <id>`
- `/memory candidates reject <id>`
- `/memory recent-auto`

Need operator logging:

- extraction counts
- promotion counts
- contradiction counts
- skipped-learning reasons
- retrieval timing / selected ids

## Rollout Plan

### Phase 0: Persistence Reliability

Deliver:

- final journal flush on clean exit
- final journal flush on single-prompt exit
- no duplicate journal rows when nothing new exists

Acceptance:

- last completed session slice is journaled even without idle timeout

### Phase 1: Auto Candidate Extraction

Deliver:

- extract constraints/decisions/preferences/open threads from journal rows
- write `memory_candidates`
- promote exact low-risk candidates immediately, leave ambiguous ones pending

Acceptance:

- repeated stable facts appear without manual `/memory write`
- noisy chatter does not create durable rows

### Phase 2: Retrieval Upgrade

Deliver:

- task-aware memory selection
- path-aware ranking
- usage logging for selected memories

Acceptance:

- irrelevant old memories stop polluting startup prompt
- current-task-relevant memories surface first

### Phase 3: Reinforcement / Contradiction

Deliver:

- usage-based reinforcement
- contradiction demotion / supersession audit
- open-thread resolution tracking

Acceptance:

- stale/wrong memories decay
- corrected facts win over older conflicting ones

### Phase 4: Maintenance / UX

Deliver:

- candidate review commands
- memory usage inspection
- maintenance compaction pass

Acceptance:

- user can inspect and steer learning without touching SQLite

## Cost Controls

- heuristic prefilter before any extraction LLM call
- journal-driven extraction first; no per-token background agent
- daily per-project extraction budget
- skip low-signal short slices
- token-bounded memory injection

## Risks

1. noise amplification
   - mitigation: candidate layer before durable promotion

2. stale memory poisoning
   - mitigation: contradiction + decay + path/task scoping

3. prompt bloat
   - mitigation: token-budgeted retrieval

4. hidden behavior
   - mitigation: observability commands + broker events

5. user trust erosion
   - mitigation: provenance on every auto-learned row

## Initial Implementation Order

1. final exit flush
2. candidate table + extraction from journal rows
3. retrieval ranking upgrade
4. memory usage logging
5. contradiction / reinforcement loop
6. user review commands

## Current Status

- Phase 0 started
- first implementation slice: guaranteed final journal flush on REPL exit
