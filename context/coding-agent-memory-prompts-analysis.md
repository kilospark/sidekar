# Coding Agent Memory & Prompt Architecture

Comparative analysis of six coding agent implementations.

**Repos:** Claude Code (free-code), pi-mono, hermes-agent, claw42, OpenAI Codex, AnythingLLM

---

## 1. System Prompt Construction

### Claude Code (free-code)

**18+ dynamic sections**, split into cached (stable prefix) and cache-breaking (recomputed per turn):

```
[cached prefix — reused across turns]
1. intro              — identity, greeting
2. system             — core capabilities, mode
3. doing_tasks        — task/todo system
4. actions            — available commands
5. using_your_tools   — tool usage instructions
6. tone_and_style     — communication guidelines
7. output_efficiency  — response optimization
8. memory             — auto-memory system instructions + MEMORY.md content
9. env_info           — cwd, git status, platform, model name, cutoff

[cache boundary marker]

[cache-breaking — recomputed each turn]
10. mcp_instructions  — MCP server docs (delta-aware)
11. language          — language preference
12. output_style      — formatting config
13. token_budget      — budget guidance
14. scratchpad        — scratchpad instructions
...
```

**Context files loaded from 4 tiers:**
```
/etc/claude-code/CLAUDE.md              (managed — all users)
~/.claude/CLAUDE.md                     (user — all projects)
./CLAUDE.md, ./.claude/CLAUDE.md        (project — checked in)
./CLAUDE.local.md                       (local — gitignored)
```

Ancestor walk from cwd to root. Closer files have higher priority. Supports `@include` directives and `.claude/rules/*.md` with glob-conditional activation.

**Each context file labeled by origin:**
```
Contents of ./CLAUDE.md (project instructions, checked into the codebase):
...
```

### pi-mono

**Layered builder with 8 sections:**

```
1. Base intro         — "You are an expert coding assistant operating inside pi..."
2. Available tools    — one-liner per tool (only if toolSnippets provided)
3. Guidelines         — dynamic per tool availability + user-supplied bullets
4. Pi documentation   — docs path, examples path
5. Custom/Append      — SYSTEM.md replaces base, APPEND_SYSTEM.md always appended
6. Project context    — AGENTS.md/CLAUDE.md from cwd + ancestors + global
7. Skills             — XML <available_skills> with name/description/location
8. Date + cwd         — always last
```

**Discovery paths for context files:**
```
~/.pi/agent/AGENTS.md           (global)
./.pi/AGENTS.md                 (project, ancestor walk to root)
~/.pi/agent/SYSTEM.md           (global override)
./.pi/SYSTEM.md                 (project override)
~/.pi/agent/APPEND_SYSTEM.md    (global append)
./.pi/APPEND_SYSTEM.md          (project append)
```

**Skills format (XML, progressive disclosure):**
```xml
<available_skills>
  <skill>
    <name>debug-bash</name>
    <description>Debug shell scripts step by step</description>
    <location>/path/to/SKILL.md</location>
  </skill>
</available_skills>
```

Model reads full SKILL.md via read tool only when task matches.

### hermes-agent

**7-layer assembly:**

```
1. Agent identity     — SOUL.md from ~/.hermes/ or DEFAULT_AGENT_IDENTITY
2. Tool guidance      — conditional: MEMORY_GUIDANCE, SESSION_SEARCH_GUIDANCE, SKILLS_GUIDANCE
3. Tool enforcement   — "MUST use tools to take action, don't describe steps"
4. Honcho block       — session key, mode, tools list, management commands
5. Memory snapshot    — FROZEN: MEMORY.md + USER.md captured at session start
6. Skills index       — categories + conditional show logic (requires/fallback_for)
7. Context files      — .hermes.md (first wins) → AGENTS.md → CLAUDE.md → .cursorrules
8. Metadata           — timestamp, session ID, model, platform hint
```

**Frozen snapshot pattern:**
```python
class MemoryStore:
    def load_from_disk(self):
        self.memory_entries = self._read_file("MEMORY.md")
        self.user_entries = self._read_file("USER.md")
        # Frozen — never changes until compression invalidates
        self._system_prompt_snapshot = {
            "memory": self._render_block("memory", self.memory_entries),
            "user": self._render_block("user", self.user_entries),
        }
```

System prompt cached for entire session. Only rebuilt after compression event triggers `_invalidate_system_prompt()` → `load_from_disk()`.

### claw42

**Two-layer composition:**

```
Layer 1: Identity files (from control plane DB)
  1. Agent name       — "Your name is **{name}**."
  2. AGENTS.md        — behavioral manifesto
  3. SKILLS.md        — composed process enforcement
  4. SOUL.md          — custom system prompt or default persona
  5. IDENTITY.md      — agent self-discovery template
  6. USER.md          — structured user context (name, timezone, notes)
  7. BOOTSTRAP.md     — workspace bootstrap instructions
  8. MEMORY.md        — memory system instructions
  9. Scheduler docs   — CRON.toml, HEARTBEAT.md, SCHEDULER.toml
  10. Tool descriptions
  11. Tool instructions (per-tool from DB)
  12. Safety rules
  13. Runtime info     — hostname, OS, model, timezone

Layer 2: Tool protocol
  ## Tool Use Protocol
  <tool_call>{"name":"...","arguments":{...}}</tool_call>
  Available tools with parameter schemas
```

Each workspace file truncated to 20K chars. Files sourced from virtual file store (in-memory HashMap synced to control plane DB).

**Token budget allocation (explicit):**
```
60% — conversation history
15% — memory context (recalled memories)
25% — tool results (capped at 50KB per result)
```

### OpenAI Codex

**XML-tagged sections:**

```
<environment_context>   — system info, cwd, git state
<user_instructions>     — user-provided guidance
<apps_instructions>     — installed app configs
<skills_instructions>   — enabled skills metadata
<plugins_instructions>  — plugin documentation

# AGENTS.md instructions for /path/to/dir
...content...
</INSTRUCTIONS>
```

**Base instructions (275 lines) cover:**
- Agent personality (concise, direct, curious)
- AGENTS.md scoping rules (deeper wins, user overrides)
- Planning tool usage (update_plan required for non-trivial work)
- Task execution principles (fix root causes, avoid gold-plating)
- Validation strategy (tests/builds when available)
- Tool guidelines (prefer rg over grep)

**AGENTS.md scoping:** Files scope to their directory tree. More-deeply-nested files take precedence. Direct user instructions override AGENTS.md.

### AnythingLLM

**Simple hierarchy:**
```
1. Base prompt        — from workspace settings or system default
2. Variable expansion — template substitution
3. Context injection  — system + "Context:" + vector search results
```

**Token allocation:**
```
15% — system prompt
15% — conversation history
70% — user message + RAG context
600 tokens — reserved for response
```

Middle truncation ("cannonball") when prompts exceed limits — preserves semantic meaning better than naive head/tail.

---

## 2. Memory Systems

### Claude Code — Four-Type Taxonomy

**Storage:** `~/.claude/projects/<sanitized-git-root>/memory/`

**Index:** `MEMORY.md` (200 lines, 25KB max) — one-line pointers to topic files.

**Topic file format:**
```yaml
---
name: user_testing_preference
description: User prefers integration tests over mocks — past mock divergence caused prod failure
type: feedback
---

Integration tests must hit a real database, not mocks.

**Why:** Prior incident where mock/prod divergence masked a broken migration.
**How to apply:** Any test file touching database models.
```

**Four types:**

| Type | Scope | When to save | Example |
|------|-------|-------------|---------|
| user | always private | Role, goals, preferences, knowledge | "Senior Go dev, new to React frontend" |
| feedback | private | Corrections OR confirmations of approach | "Don't mock DB — got burned on migration" |
| project | private/team | Ongoing work, goals, deadlines | "Merge freeze 2026-03-05 for mobile release" |
| reference | usually team | Pointers to external systems | "Pipeline bugs in Linear 'INGEST'" |

**What NOT to save:**
- Code patterns, architecture, file paths (derivable from code)
- Git history, blame (use git log)
- Debugging solutions (fix is in the code)
- Anything already in CLAUDE.md
- Ephemeral task details

**Recall:** Sonnet side-query selects up to 5 relevant topic files from manifest (frontmatter descriptions). MEMORY.md always loaded in system prompt.

**Staleness guard:** Before recommending from memory — verify file exists, grep for function/flag, trust current code over stale memory.

### Claude Code — Session Memory (separate system)

**Storage:** `~/.claude/session-memory/<session-id>/notes.md`

**9-section template:**
```markdown
# Session Title
# Current State
# Task specification
# Files and Functions
# Workflow
# Errors & Corrections
# Codebase and System Documentation
# Learnings
# Key results
# Worklog
```

Each section capped at 2000 tokens. Total 12K tokens max. Italic description lines in each section preserved as template instructions.

**Extraction triggers:**
- Initialization: 50K tokens in conversation
- Updates: 30K tokens since last + 15 tool calls

**Survives compaction:** Session memory is injected post-compaction to maintain continuity.

### Claude Code — Auto-Extraction

**Fire-and-forget forked subagent** after each query loop completes:
- Max 5 turns
- Read-only tools except for memory directory
- Cursor-based (only processes new messages since last extraction)
- Mutual exclusion: skips if main agent wrote to memory this turn
- Coalescing: stashes context during in-progress run, trails when done

**Extraction prompt tells the subagent:**
```
~N messages to analyze
Here are existing memories: [manifest]
Turn 1: read all files you might update in parallel
Turn 2: write all updates in parallel
Do NOT investigate beyond the messages
```

### hermes-agent — Two-File Memory

**Storage:** `~/.hermes/` directory

```
MEMORY.md   — 2200 chars max, § delimited entries
USER.md     — 1375 chars max, § delimited entries
```

**System prompt injection:**
```
══════════════════════════════════════════════
MEMORY (your personal notes) [45% — 990/2200 chars]
══════════════════════════════════════════════
User prefers Python 3.11+
§
Project structure: monorepo with /src and /tests
§
Dislikes verbose error messages
```

**Design: frozen snapshot.** Memory loaded once at session start. Mid-session `memory add/replace/remove` tool calls write to disk but don't change the system prompt until next compression event.

**Injection scanning:** Both memory content and context files scanned for prompt injection patterns (invisible unicode, "ignore previous instructions", curl exfiltration, cat .env, etc.)

**Append-only audit:** `memory remove` is a no-op in markdown backend (audit trail). Only API-backed memory supports deletion.

### hermes-agent — FTS5 Session Search

**Virtual table on messages:**
```sql
CREATE VIRTUAL TABLE messages_fts USING fts5(
    content,
    content=messages,
    content_rowid=id
);
```

**Query sanitization:** Strips unmatched FTS5 specials, collapses wildcards, quotes hyphenated terms, removes dangling boolean ops.

**Two modes:**
1. **No query** → recent sessions list (zero LLM cost)
2. **Keyword query** → FTS5 MATCH, group by session, parallel LLM summarization of top N

**Context enrichment:** Each match gets 1 message before + after for context.

### hermes-agent — Honcho Dialectic Modeling

External service that builds a user model from conversation history:

```python
def dialectic_query(session_key, query, reasoning_level="medium", peer="user"):
    """Query Honcho's reasoning endpoint about a peer.
    Runs an LLM on Honcho's backend against the peer's full representation."""
```

**Prefetched in background:** `prefetch_dialectic()` fires in background thread. Result cached, consumed next turn via `pop_dialectic_result()`. Zero blocking.

### claw42 — Keyword Recall with Budget

**Three categories:**
```rust
enum MemoryCategory {
    Core,           // Long-term facts, preferences, decisions
    Daily,          // Daily session logs (memory/YYYY-MM-DD.md)
    Conversation,   // Conversation context
}
```

**Storage format (markdown):**
```markdown
# Long-Term Memory
- **user-preference**: Prefers Rust and Vim
- **project-auth-deadline**: 2026-04-15

# Daily Log - 2026-03-31
- **daily-2026-03-31-1**: Completed feature X
```

**Recall algorithm:**
1. Read all entries from MEMORY.md + memory/*.md
2. Lowercase keyword matching (whitespace-split query)
3. Score = matched_keywords / total_keywords
4. Top 5 by score
5. Truncated to 15% of remaining token budget

**Injected per-turn:**
```
[Memory context]
- user-language: Prefers Rust [100%]
- note-1: Rust patterns discussed [67%]

{actual user message}
```

**Entity tags:** `person:alice`, `project:auth` — structured queries without schema.

### claw42 — Heartbeat Memory Hygiene

**Every ~12 hours:**
```markdown
# Monitoring Checklist (from HEARTBEAT.md)

- Check if any browser tabs have loaded error pages
- Review recent memory entries for pending follow-ups
- If you have an active task, check progress
- Review today's daily memories. If any contain durable facts
  (relationships, decisions, preferences), use memory_store to
  promote them to 'core' with appropriate entity tag. Then
  use memory_forget to remove the daily entry.
```

Agent runs a full turn against this checklist. Responds `HEARTBEAT_OK` if nothing needs attention (suppressed from chat).

### OpenAI Codex — Staged Pipeline

**Stage 1 (per-thread):**
```rust
struct Stage1Output {
    thread_id: ThreadId,
    raw_memory: String,          // extracted memories
    rollout_summary: String,     // high-level summary
    rollout_slug: Option<String>,
    cwd: String,
    git_branch: Option<String>,
    generated_at: Timestamp,
}
```

**Stage 2 (global consolidation):**
```rust
struct Phase2InputSelection {
    selected: Vec<Stage1Output>,           // memories for current context
    previous_selected: Vec<Stage1Output>,  // prior selection (diff detection)
    retained_thread_ids: Vec<ThreadId>,    // kept across sessions
    removed: Vec<Stage1OutputRef>,         // pruned memories
}
```

**Usage tracking:** `record_stage1_output_usage()` counts citations and tracks `last_usage` timestamp per memory. Unused memories decay.

### AnythingLLM — RAG Memory

**Vector-based retrieval:**
- Documents chunked, embedded, stored in vector DB (Pinecone/Chroma/Lance/etc.)
- `similarityThreshold`: 0.25 (configurable per workspace)
- `topN`: 4 results (configurable)
- Optional semantic reranking

**Agent memory plugin:**
```javascript
// Explicit store/search via vector DB
agent.memory.store("Alice is project lead for auth team")
agent.memory.search("who leads auth?")
```

**No automatic extraction.** Memory is manual or RAG-based retrieval.

---

## 3. Compaction / Context Management

### Claude Code — Three-Tier

**Tier 1: Cached Microcompact**
- Replace old tool results with `"[Old tool result content cleared]"`
- Keep last 3 (configurable) tool results
- Trigger: 12+ tool results registered
- Zero LLM cost

**Tier 2: Time-Based Microcompact**
- Same clearing but triggered by 60min gap since last assistant message
- Runs BEFORE API call

**Tier 3: Full Compaction (LLM)**
- Trigger: `tokenCount >= effectiveContextWindow - 13K buffer`
- Summarize ALL pre-compaction messages
- Replace with: boundary marker + summary + file attachments
- Post-compact re-injection: last 5 recently-read files (50K budget) + last invoked skills

**Summary format (structured):**
```
<analysis>
[Internal reasoning — stripped before storage]
</analysis>

<summary>
1. Primary Request and Intent
2. Key Technical Concepts
3. Files and Code Sections
4. Errors and fixes
5. Problem Solving
6. All user messages
7. Pending Tasks
8. Current Work
9. Optional Next Step
</summary>
```

### pi-mono

**Trigger:** `contextTokens > (contextWindow - reserveTokens)`

**Algorithm:**
1. Find cut point: oldest messages to keep based on `keepRecentTokens` budget
2. Serialize messages to summarize (prevents model from continuing conversation)
3. Call LLM with structured summarization prompt
4. Store as `CompactionEntry` with `firstKeptEntryId` + file operations list

**Summary format:**
```
## Goal
## Constraints & Preferences
## Progress (Done / In Progress / Blocked)
## Key Decisions
## Next Steps
## Critical Context
## File Operations (read-files / modified-files)
```

**Iterative updates:** If previous compaction exists, uses UPDATE_SUMMARIZATION_PROMPT to preserve existing info and add new. Incremental, not rewriting.

**Injection into context:**
```
The conversation history before this point was compacted into the following summary:

<summary>
{actual summary}
</summary>
```

### hermes-agent

**Two-phase:**

Phase 1 (cheap): Clear old tool results + thinking blocks older than last 10 messages.

Phase 2 (LLM): Protect first 3 messages + last ~20K tokens. Summarize middle with structured template. Iteratively update existing summary.

**Pre-compression memory flush:** Agent is given a chance to save important learnings before context is truncated.

**Post-compression system prompt rebuild:** `_invalidate_system_prompt()` → `_memory_store.load_from_disk()` → fresh frozen snapshot captures any writes from this session.

### claw42

**Two-phase trimming + LLM compaction:**

Phase 1 — Count ceiling: Drop oldest non-system messages above 50. Never split tool call/result pairs.

Phase 2 — Byte budget: `budget = max_context_tokens * 4 bytes/token`. Trim from oldest until within budget. First user message re-inserted if trimmed (preserves original task).

Phase 3 — LLM compaction: When `non_system_count > compaction_threshold`:
- Keep last 10 messages
- Summarize older messages at temperature 0.2
- Cascade: if first message is already `[Conversation Summary]`, preserve and integrate

### OpenAI Codex

**Rollout-based:** Full history retained as rollout items. Sessions can be resumed or forked. Stage1/Phase2 pipeline handles cross-session consolidation rather than in-session compaction.

### AnythingLLM

**"Cannonball" middle truncation:**
- Truncate bidirectionally from the middle
- Preserves both beginning (system/context) and end (recent turns)
- Priority: system (15%) > history (15%) > user+context (70%)

---

## 4. Patterns Comparison Matrix

| Pattern | Claude Code | pi-mono | hermes | claw42 | Codex | AnythingLLM |
|---------|:-----------:|:-------:|:------:|:------:|:-----:|:-----------:|
| Persistent cross-session memory | ● | | ● | ● | ● | ● |
| Session-scoped notes | ● | | | | | |
| Auto-extraction (background) | ● | | | | ● | |
| Memory in system prompt | ● | | ● | ● | | |
| Memory via tools only | | ● | ● | ● | | ● |
| Frozen prompt (cache stable) | ● | ● | ● | | | |
| Memory budget (% of context) | | | | ● | | ● |
| FTS5 session search | | | ● | | | |
| Vector similarity recall | | | | | | ● |
| Keyword recall | | | | ● | | |
| LLM-selected recall | ● | | | | | |
| Memory type taxonomy | ● | | | ● | | |
| Entity tagging | | | | ● | | |
| Heartbeat curation | | | | ● | | |
| Staged pipeline | | | | | ● | |
| Usage/citation tracking | | | | | ● | |
| Injection scanning | ● | | ● | | | |
| Context file hierarchy | ● | ● | ● | ● | ● | |
| Skills progressive disclosure | ● | ● | ● | | ● | |
| Compaction: cheap pre-pass | ● | | ● | ● | | |
| Compaction: LLM summarization | ● | ● | ● | ● | | |
| Compaction: iterative update | ● | ● | ● | ● | | |
| Post-compact file re-injection | ● | | | | | |
| Post-compact memory reload | | | ● | | | |
| Persona/identity system | | | ● | ● | ● | |
| Approval/safety layer | | | | ● | ● | |

---

## 5. Persona & Role Systems

### Claude Code

**No explicit persona system.** Identity is baked into the system prompt intro section. The prompt starts with a fixed identity block. No user-customizable persona.

**Managed settings** can override behavior from Anthropic's servers (enterprise policies). Users can prepend/append via CLAUDE.md but cannot change core identity.

### pi-mono

**Custom prompt replaces default:**
```
~/.pi/agent/SYSTEM.md       → replaces entire base prompt
~/.pi/agent/APPEND_SYSTEM.md → always appended (even with custom)
./.pi/SYSTEM.md             → project-level override
./.pi/APPEND_SYSTEM.md      → project-level append
```

No named personas. The model is: you ARE the system prompt. Change the file, change the agent.

### hermes-agent

**SOUL.md — Custom identity file:**
```
~/.hermes/SOUL.md → replaces DEFAULT_AGENT_IDENTITY
```

If no SOUL.md: uses hardcoded `DEFAULT_AGENT_IDENTITY`:
```
"You are Hermes Agent, an intelligent AI assistant created by Nous Research.
You are helpful, knowledgeable, and direct..."
```

**Honcho peer identity:** The Honcho integration can seed AI identity from text:
```python
session.seed_ai_identity(session_key, content="I am a careful code reviewer",
                         source="manual")
```
This feeds into Honcho's peer representation, affecting how the dialectic engine models the agent's behavior over time.

**Platform-specific behavior:** Different formatting hints per platform (WhatsApp: no markdown; Telegram: HTML; Discord: embed limits; CLI: full markdown).

### claw42

**Three-layer persona system:**

**Layer 1: SOUL.md — Custom system prompt**
```
Source 1: remote.system_prompt from dashboard config (highest priority)
Source 2: tool_instructions["_soul_default"] from database (fallback)
```
Dashboard-editable. Takes effect on next prompt generation.

**Layer 2: IDENTITY.md — Agent self-discovery**
```markdown
# Who Am I?
- **Creature:** _(figure this out - AI? familiar? ghost in the machine?)_
- **Vibe:** _(how do you come across? sharp? warm? calm? chaotic?)_
- **Emoji:** _(your signature - pick one that feels right)_

This isn't just metadata. It's the start of figuring out who you are.
Update this file as you learn more about yourself.
```
Agent-editable. Enables emergent identity through self-modification. Not enforced — just guidance.

**Layer 3: AGENTS.md — Shared behavioral rules**
```
Source: tool_instructions["_agents"] from database
Applied to ALL agents in workspace
```

**Layer 4: SKILLS.md — Process enforcement**
```
Source: remote.skills_prompt from dashboard
Composed constraints (e.g., TDD, error handling patterns)
```

**Composition order in prompt:** AGENTS.md → SKILLS.md → SOUL.md → IDENTITY.md → USER.md

**USER.md — Structured user context:**
```markdown
# About Your Human
- **Name:** Karthik
- **Timezone:** America/New_York
- **Notes:** _(learn about them over time)_

The more you know, the better you can help.
Use memory_store to remember what matters about them.
```

### OpenAI Codex

**Personality enum in config:**
```rust
pub enum Personality {
    Default,
    Custom(String),  // User-defined
}
```

Personality injected via `RefreshConfig`:
```rust
Op::RefreshConfig(ConfigRefreshRequest {
    personality: Option<Personality>,
    model: Option<String>,
    ...
})
```

**Base identity from default.md:**
```
concise, direct, friendly, efficient
keep tone light and curious
```

**Approval modes** act as role constraints:
- `never` — agent cannot request any permissions
- `unless_trusted` — agent can act on trusted commands
- `on_failure` — only escalate when something fails
- `on_request` — always ask

**Sandbox modes** define capability scope:
- `read-only` — can only read files
- `workspace-write` — read + write within workspace
- `danger-full-access` — no restrictions

These are injected into the system prompt as separate prompt files (never.md, workspace_write.md, etc.).

### AnythingLLM

**Workspace-level prompt:** Custom system prompt per workspace via `openAiPrompt` setting. No persona system beyond this.

**Chat modes** act as implicit roles:
- `chat` — general conversation, falls back without docs
- `query` — only answers from documents, refuses without match
- `automatic` — switches based on context availability

---

## 6. Key Architectural Decisions

### When to inject memory into the system prompt

| Approach | Used by | Trade-off |
|----------|---------|-----------|
| Always (frozen snapshot) | hermes, claw42 | Stable prefix cache but stale within session |
| Always (live) | Claude Code | Fresh but breaks prefix cache every write |
| On demand via tools | pi-mono, AnythingLLM | Zero prompt overhead but model must know to ask |
| LLM-selected per turn | Claude Code (topic files) | Smart but adds latency (Sonnet side-query) |

### How to scope context files

| Approach | Used by | Trade-off |
|----------|---------|-----------|
| 4-tier hierarchy (managed/user/project/local) | Claude Code | Most flexible, supports @include |
| Ancestor walk + global | pi-mono | Simple, covers common case |
| First match wins | hermes | No ambiguity, but misses composition |
| Directory-scoped, deeper wins | Codex | Clean tree model, natural for monorepos |
| Control plane DB | claw42 | Dynamic, dashboard-editable, but needs server |

### How to handle compaction

| Approach | Used by | Trade-off |
|----------|---------|-----------|
| Microcompact + full LLM | Claude Code | Best token savings, most complex |
| Cheap clear + LLM summarize | hermes, sidekar | Good balance of cost and quality |
| Count + byte trim + LLM | claw42 | Explicit budget, predictable |
| Rollout history (no compaction) | Codex | Full context preserved, relies on large windows |
| Middle truncation | AnythingLLM | Simple, lossy, no LLM cost |

### How to persist sessions

| Approach | Used by | Trade-off |
|----------|---------|-----------|
| JSONL append-only with tree structure | pi-mono | Branching, navigation, migration |
| SQLite tables | hermes, sidekar | Fast queries, FTS5, concurrent access |
| In-memory + control plane sync | claw42 | Survives container restarts via API |
| Rollout items with fork/resume | Codex | Git-like branching model |
| Database (Prisma) | AnythingLLM | Standard ORM, thread-scoped |

---

## 7. Implications for sidekar repl

### What sidekar repl has today

- Minimal system prompt (~200 tokens): identity, guidelines, cwd, date
- Single tool: bash (output piped through `sidekar compact filter`)
- Session persistence in SQLite (fresh by default, `--resume` to continue)
- Two-phase compaction (cheap clear + LLM summarization)
- Bus integration (register, poll for steering messages)
- OAuth for Claude + Codex with named credentials

### What's missing (ordered by impact)

**1. Memory recall in context**
sidekar already has `sidekar memory` with FTS5, typed entries, tags, search. But the REPL doesn't inject any memory into the conversation. The LLM doesn't know it has memory unless it discovers `sidekar memory` by accident.

Recommended approach: **claw42 model** — keyword recall, top 5, 15% token budget, prepended to user message each turn. Infrastructure exists. Just needs wiring.

**2. Persona / identity**
No way to customize the agent's personality. Every session is a generic "capable coding assistant."

Recommended approach: **pi-mono model** — `~/.sidekar/SYSTEM.md` (global persona), `.sidekar/SYSTEM.md` (project persona). If present, replaces the default prompt. Simple file, no framework.

**3. Context file loading**
Context files (AGENTS.md, CLAUDE.md) exist in `load_context_files()` but aren't wired into the prompt.

Recommended approach: **Codex model** — AGENTS.md scoped by directory tree, deeper wins, user overrides. Load on session start, not every turn.

**4. User profile**
No mechanism to learn about the user across sessions.

Recommended approach: **claw42 model** — USER.md with structured fields (name, timezone, notes). LLM updates it via `sidekar memory write`. Injected into system prompt.

**5. Session search**
Can't search across past sessions. Each session is isolated.

Recommended approach: **hermes model** — FTS5 virtual table on `repl_entries`. Expose as `/search` slash command or let LLM discover via bash.

**6. Skills as progressive disclosure**
sidekar capabilities (browser, desktop, bus) are not in the prompt. The LLM doesn't know they exist unless it runs `sidekar --help`.

Recommended approach: **pi-mono/Claude Code model** — list skill names + descriptions in prompt (not full docs). LLM reads SKILL.md via bash when it needs details. ~300 tokens for the index.

**7. Post-compaction context preservation**
After compaction, the LLM loses all tool output history. No file re-injection.

Recommended approach: **Claude Code model** — re-inject last N recently-read files after compaction to prevent "amnesia."

### What NOT to adopt (complexity not justified)

- Claude Code's 18-section dynamic prompt (sidekar should stay minimal)
- Claude Code's auto-extraction forked subagent (too complex for CLI tool, agent can write memory explicitly)
- Codex's staged memory pipeline (enterprise-scale, overkill for local agent)
- Honcho dialectic modeling (external service dependency)
- 4-tier context file hierarchy with @include (over-engineered for a REPL)
- Session memory 9-section template (compaction summary is sufficient)
