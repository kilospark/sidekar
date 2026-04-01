# REPL Memory & System Prompt Design Discussion

Date: 2026-04-01 (updated)

Cross-repo analysis of system prompts and memory mechanisms from six coding agent implementations. Applied to sidekar repl design.

Repos analyzed:
- **pi-mono** — badlogic/pi-mono (TypeScript, Anthropic/OpenAI/Google)
- **free-code** — paoloanzn/free-code (Claude Code source, TypeScript/Bun)
- **hermes-agent** — NousResearch/hermes-agent (Python, multi-provider)
- **anything-llm** — Mintplex-Labs/anything-llm (Node.js, RAG-focused)
- **claw42** — kilospark/claw42 (Rust agent + Next.js dashboard, multi-provider)
- **codex** — openai/codex (Rust + TypeScript, OpenAI Codex CLI)

---

## System Prompt Architecture

| | **Claude Code** | **pi-mono** | **hermes** | **claw42** | **Codex** | **AnythingLLM** |
|---|---|---|---|---|---|---|
| **Structure** | 18+ dynamic sections, cached vs cache-breaking per turn | Layered builder (intro, tools, guidelines, context, skills, date) | 7-layer assembly (identity, tool guidance, honcho, memory, skills, context, metadata) | Two-layer: identity files (SOUL/AGENTS/SKILLS/USER/IDENTITY.md) + tool instructions | XML-tagged sections (environment, user, apps, skills, plugins, AGENTS.md) | Simple: base + RAG context |
| **Context files** | CLAUDE.md from 4 tiers (managed/user/project/local) + ancestor walk + @include + .claude/rules/*.md | AGENTS.md/CLAUDE.md from cwd + ancestors + global | .hermes.md first → AGENTS.md → CLAUDE.md → .cursorrules | AGENTS.md + SOUL.md + SKILLS.md from control plane DB | AGENTS.md scoped per directory tree, deeper files take precedence, user instructions override | Workspace settings only |
| **Skills** | XML `<available_skills>`, model reads full file on demand | Same XML format, disableModelInvocation flag | Markdown index, conditional show (requires/fallback_for) | SKILLS.md from dashboard, composed process enforcement | Skills as `<skills_instructions>` tag | N/A |
| **Caching** | Cache boundary marker, cached prefix reused across turns | Rebuilt only on tool change or extension reload | Frozen snapshot, rebuilt only after compression | Dynamic per-turn (no caching, fresh prompt each turn) | Not documented | N/A |
| **Persona** | N/A (identity in prompt) | Custom prompt replaces default | SOUL.md or DEFAULT_AGENT_IDENTITY | SOUL.md (custom system prompt or _soul_default from DB), IDENTITY.md (agent self-discovery) | Personality enum in config | N/A |

## Memory Systems

| | **Claude Code** | **pi-mono** | **hermes** | **claw42** | **Codex** | **AnythingLLM** |
|---|---|---|---|---|---|---|
| **Persistent memory** | 4-type taxonomy (user/feedback/project/reference), markdown+frontmatter in ~/.claude/projects/ | None (compaction only) | 2 files: MEMORY.md (2200 chars) + USER.md (1375 chars), § delimited | Keyword-search recall, append-only markdown (MEMORY.md + daily logs), categories: core/daily/conversation | Staged pipeline: Stage1 extraction → Phase2 global consolidation, SQLite-backed | Vector DB plugin |
| **Session memory** | 9-section template (12K cap), extracted at token thresholds | Compaction entries in JSONL | FTS5 cross-session search + Honcho dialectic modeling | Per-session history in HashMap, trimmed to 50 msgs, compacted with LLM | Rollout system with resume/fork, cross-session message history | Last N messages |
| **Auto-extraction** | Forked subagent (max 5 turns), fire-and-forget, mutually exclusive with main agent | None | None (agent writes explicitly) | Heartbeat promotes daily→core memories every ~12h | Stage1 extraction per thread, Phase2 consolidation global | None |
| **Memory recall** | Sonnet side-query selects up to 5 relevant files from manifest | buildSessionContext() injects compaction summaries | Frozen snapshot in system prompt, live writes don't change until compression | Top 5 keyword-matched entries prepended to user message (15% token budget) | Phase2InputSelection with usage tracking (citation counting) | Vector similarity search |
| **Memory budget** | MEMORY.md: 200 lines/25KB, topic files unlimited | N/A | MEMORY.md: 2200 chars, USER.md: 1375 chars | 15% of remaining token budget for memory context | Not documented | top-K results |

## Heartbeat & Background

| | **claw42** | **hermes** | **Codex** | Others |
|---|---|---|---|---|
| **Heartbeat** | Every 30min, reads HEARTBEAT.md checklist, runs full agent turn, suppresses if HEARTBEAT_OK | N/A | N/A | N/A |
| **Memory hygiene** | Every ~12h, promotes daily memories to core, prunes stale entries | N/A | Stage1/Phase2 pipeline consolidation | N/A |
| **Events** | ON_EVENT.md handlers (e.g., on_title_change), debounced 3s, non-blocking | N/A | N/A | N/A |
| **Cron** | CRON.toml, 5-field schedule, state persisted, re-parses each iteration | cron scheduler with platform delivery | N/A | N/A |

## History & Compaction

| | **Claude Code** | **pi-mono** | **hermes** | **claw42** | **Codex** | **AnythingLLM** |
|---|---|---|---|---|---|---|
| **Trigger** | Auto at contextWindow - reserveTokens | Auto at contextWindow - reserveTokens | 50% of context window | Count (>30 msgs) + configurable threshold | Not documented (rollout-based) | Token limit |
| **Strategy** | Microcompact (clear old tool results) + full summarization | LLM summarizes oldest turns, keeps recent by token budget | Phase 1: clear old results. Phase 2: LLM structured summary | Keep last 10, serialize+summarize older, cascade existing summaries | Resume/fork model (keeps full rollout history) | Middle truncation ("cannonball") |
| **Summary format** | 9 sections: Goal, Constraints, Progress, Decisions, Next Steps, Critical Context, File Ops | Goal, Constraints, Progress (Done/InProgress/Blocked), Key Decisions, Next Steps, Critical Context | Same structured template | Single `[Conversation Summary]` block, low temperature (0.2) | Per-thread Stage1 extraction (raw_memory + rollout_summary) | N/A |
| **Preservation** | Previous summary iteratively updated | Previous summary preserved and updated | Previous summary integrated | Cascading: existing summary preserved and integrated | Rollout history retained, forked | Priority: user > system > history |

## Token Budget Allocation (claw42)

claw42 has the most explicit budget system:
- **60%** history (conversation turns)
- **15%** memory context (recalled memories)
- **25%** tool results (capped at 50KB per result)
- System prompt and first user message get remainder

## Key Design Patterns

### 1. Frozen snapshot + compression-triggered reload (hermes, claw42)
System prompt cached for prefix cache stability. Memory writes mid-session don't change the prompt. Only after compaction does memory reload. claw42 does this implicitly — memory snapshot is built once at turn start.

### 2. Four-type memory taxonomy (Claude Code)
user/feedback/project/reference with "what NOT to save" rules. Description field enables relevance-based recall without reading full files. Most sophisticated deduplication of the six.

### 3. Auto-extraction via forked subagent (Claude Code)
Fire-and-forget background extraction. Max 5 turns. Read-only tools except memory dir. Cursor-based. Coalescing prevents duplicates. Highest automation level.

### 4. Heartbeat memory hygiene (claw42)
Background timer promotes ephemeral daily memories to durable core storage. Self-healing checks (CDP, disk). Only implementation that actively curates its own memory.

### 5. Staged memory pipeline (Codex)
Stage1 per-thread extraction → Phase2 global consolidation. Usage tracking (citation counting). Most structured pipeline for multi-session memory.

### 6. AGENTS.md scoped by directory (Codex)
AGENTS.md files scope to their directory tree. Deeper files take precedence. Direct user instructions override all. Clean hierarchical model.

### 7. Keyword recall with budget (claw42)
Simple but effective: keyword match, score by fraction of matches, top 5, capped at 15% of token budget. No LLM needed for recall.

### 8. Session search with FTS5 + LLM summarization (hermes)
Raw FTS5 is fast; expensive LLM summarization only on top N results. Empty query = instant recent sessions.

### 9. Append-only memory with entity tags (claw42)
Audit trail by design — forget is a no-op for markdown backend. Entity tags (person:alice, project:auth) enable structured queries without a schema.

### 10. Context fragment tags (Codex)
XML tags (`<environment_context>`, `<user_instructions>`, `<skills_instructions>`) wrap each context section. Clean separation, easy to strip or replace.

---

## Adoption Priority for sidekar repl

| # | Feature | From | Effort | Why |
|---|---------|------|--------|-----|
| 1 | Memory recall injected into user message | claw42 | Small | sidekar already has memory; just prepend top matches before user msg |
| 2 | Frozen prompt with compression reload | hermes/claw42 | Small | Cache stability, already nearly there |
| 3 | FTS5 on session entries | hermes | Small | Already have SQLite table, add virtual table |
| 4 | AGENTS.md scoped loading | Codex | Small | Directory-scoped, deeper wins, user overrides |
| 5 | Token budget allocation | claw42 | Medium | Explicit %s for history/memory/tools |
| 6 | Heartbeat memory promotion | claw42 | Medium | Background daily→core curation |
| 7 | Auto-memory extraction | Claude Code | Large | Forked subagent pattern |
| 8 | Staged memory pipeline | Codex | Large | Multi-phase extraction + consolidation |

---

## Open Design Questions

### Memory approach for sidekar repl
sidekar already has `sidekar memory` (FTS5, typed entries, tags, search, compact). Options:
1. **Inject top matches** — like claw42, prepend recalled memories to user message each turn (15% budget)
2. **Tools only** — LLM calls `sidekar memory search/write` through bash on demand
3. **Hybrid** — inject a brief on session start, tools for explicit save/search

### Context file loading
Stripped from system prompt. Options:
1. **Codex model** — AGENTS.md scoped by directory, deeper wins, auto-loaded
2. **On demand** — LLM reads context files when it needs them via bash
3. **Skill-triggered** — context loading as a discoverable skill

### Persona system
claw42 has SOUL.md (custom system prompt) + IDENTITY.md (agent self-discovery). Could enable:
- `~/.sidekar/SOUL.md` — global personality
- `.sidekar/SOUL.md` — project-specific persona
- Identity emergence through self-modification

### Session memory vs persistent memory
Claude Code separates session notes (9-section template, 12K cap) from persistent memory (4-type files). claw42 has daily logs that get promoted to core. Question: should sidekar repl have session-scoped notes that survive compaction?
