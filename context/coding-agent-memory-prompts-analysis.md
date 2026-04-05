# Coding Agent Memory & Prompt Architecture

Comparative analysis of seven coding agent implementations.

**Repos:** Claude Code (free-code), pi-mono, hermes-agent, claw42, OpenAI Codex, AnythingLLM, opencode

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

### opencode

**Model-family-specific system prompt selection:**
```typescript
// session/system.ts
function provider(model) {
  if (model.api.id.includes("gpt-4") || "o1" || "o3") return [PROMPT_BEAST]
  if (model.api.id.includes("gpt") && "codex") return [PROMPT_CODEX]
  if (model.api.id.includes("gpt")) return [PROMPT_GPT]
  if (model.api.id.includes("gemini-")) return [PROMPT_GEMINI]
  if (model.api.id.includes("claude")) return [PROMPT_ANTHROPIC]
  if (model.api.id.includes("trinity")) return [PROMPT_TRINITY]
  if (model.api.id.includes("kimi")) return [PROMPT_KIMI]
  return [PROMPT_DEFAULT]
}
```

Eight hand-tuned base prompts, chosen by model ID substring. Most donors use one base prompt. opencode is the only one that admits different frontier models need different phrasing for the same behavior.

**Composition layers:**
```
1. Model-family base prompt (one of 8)
2. Environment block — cwd, worktree, git state, platform, date, directory tree
3. Skills block — verbose XML <available_skills> with name/description/location
4. Agent-specific prompt override (optional, per selected agent)
5. Context files — AGENTS.md scoped by directory (Codex-style)
```

**Agent-scoped prompts:** Each agent (`build`, `plan`, `general`, `explore`, `compaction`, `title`, `summary`) can ship its own system prompt file (e.g., `PROMPT_EXPLORE` for the explore subagent). The hidden `compaction` / `title` / `summary` agents are specialized single-purpose prompts — opencode models them as full agents rather than ad-hoc LLM calls.

**ACP (Agent Client Protocol):** Has a dedicated `acp/` module implementing an agent protocol so external clients can drive opencode as a backend. None of the other donors expose their agent as a protocol-speaking server in this form.

**Effect-based service layer:** Uses the TypeScript `effect` library for service composition (`Skill.Service`, `Agent.Service`, `Config.Service`, etc.) — distinctive architecturally vs. the other donors, which use plain classes or closures.

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

### opencode — None (session-only)

**No persistent cross-session memory.** Searched `packages/opencode/src` for persistent-memory modules — nothing. No MEMORY.md, no event store, no auto-extraction, no cross-session recall, no keyword/FTS search over past conversations.

What it does have is **per-session lifecycle tooling**:
- `session/compaction.ts` (428 lines) — in-session context pruning
- `session/summary.ts` — session summarization via a dedicated hidden `summary` agent
- `session/overflow.ts` — overflow detection
- Hidden `title` agent — generates session titles
- `PRUNE_PROTECTED_TOOLS = ["skill"]` — skill tool calls are protected from pruning during compaction so recently-loaded skill content survives

Memory across sessions is the user's responsibility: start a new session, re-state context. The AGENTS.md hierarchy (inherited from Codex) carries durable per-project context, but there's no project-scoped memory event store.

**Implication:** If you're evaluating opencode as a donor pattern, the memory half of this document is not where it contributes. Its strengths are the agent system (§ Background Agent Support) and the skills discovery path.

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

### opencode — Hidden-agent compaction

**Constants (from `session/compaction.ts`):**
```typescript
const PRUNE_MINIMUM = 20_000           // don't prune below this
const PRUNE_PROTECT = 40_000           // preserve last ~40K tokens from pruning
const PRUNE_PROTECTED_TOOLS = ["skill"] // always keep skill tool calls
```

**Compaction runs inside a dedicated hidden agent.** opencode treats compaction as an agent rather than a bespoke LLM call: the `compaction` agent is a native agent with `mode: "primary"`, `hidden: true`, its own `PROMPT_COMPACTION` system prompt, and a "deny all" permission set so it can only summarize, not act. Same pattern for `title` and `summary` agents.

**Separation of concerns:**
- `overflow.ts` — detects when to compact (overflow threshold)
- `compaction.ts` — orchestrates pruning + summarization
- `summary.ts` — invoked by the hidden `summary` agent to produce session summaries
- The hidden `title` agent generates session titles at a different trigger point

**Why this is interesting:** every other donor has compaction as either (a) inline logic in the session loop or (b) a hardcoded LLM call. opencode promotes it to a first-class agent concept, which means the same compaction prompt/permission model can be tested, swapped, or per-model-tuned the same way any other agent can.

---

## 4. Patterns Comparison Matrix

| Pattern | Claude Code | pi-mono | hermes | claw42 | Codex | AnythingLLM | opencode |
|---------|:-----------:|:-------:|:------:|:------:|:-----:|:-----------:|:--------:|
| Persistent cross-session memory | ● | | ● | ● | ● | ● | |
| Session-scoped notes | ● | | | | | | |
| Auto-extraction (background) | ● | | | | ● | | |
| Memory in system prompt | ● | | ● | ● | | | |
| Memory via tools only | | ● | ● | ● | | ● | ● |
| Frozen prompt (cache stable) | ● | ● | ● | | | | ● |
| Memory budget (% of context) | | | | ● | | ● | |
| FTS5 session search | | | ● | | | | |
| Vector similarity recall | | | | | | ● | |
| Keyword recall | | | | ● | | | |
| LLM-selected recall | ● | | | | | | |
| Memory type taxonomy | ● | | | ● | | | |
| Entity tagging | | | | ● | | | |
| Heartbeat curation | | | | ● | | | |
| Staged pipeline | | | | | ● | | |
| Usage/citation tracking | | | | | ● | | |
| Injection scanning | ● | | ● | | | | |
| Context file hierarchy | ● | ● | ● | ● | ● | | ● |
| Skills progressive disclosure | ● | ● | ● | | ● | | ● |
| Skills cross-agent discovery | | | ● | | | | ● |
| Compaction: cheap pre-pass | ● | | ● | ● | | | ● |
| Compaction: LLM summarization | ● | ● | ● | ● | | | ● |
| Compaction: iterative update | ● | ● | ● | ● | | | |
| Compaction: dedicated agent | | | | | | | ● |
| Post-compact file re-injection | ● | | | | | | |
| Post-compact memory reload | | | ● | | | | |
| Persona/identity system | | | ● | ● | ● | | |
| Approval/safety layer | | | | ● | ● | | ● |
| **Background / parallel subagents** | ● | | ● | ● | ● | | |
| **Plan vs build mode (first-class)** | ● | ● | | | ● | | ● |
| **Plan mode (via skill/slash)** | | | ● | | | | |
| **Agent-as-tool (spawn from LLM)** | ● | | ● | ● | ● | | |
| **Agent-as-protocol (ACP)** | | | | | | | ● |
| **Per-agent permission scoping** | ● | | ● | | ● | | ● |
| **Skills: file-based SKILL.md** | ● | ● | ● | | ● | | ● |
| **Skills: remote registry/hub** | | | ● | | | | ● |
| **Skills: conditional activation** | ● | ● | ● | | | | ● |
| **Skills: env var dependencies** | | | ● | ● | | | |

---

## 5. Background Agent Support

### Claude Code — Fork + Teammate Spawning

**Two paths for background agents:**

**Path 1: Fork subagents** (feature-gated, implicit context inheritance):
```typescript
const FORK_AGENT = {
  agentType: 'fork',
  tools: ['*'],        // Exact parent toolset
  maxTurns: 200,
  model: 'inherit',    // Parent's model
  permissionMode: 'bubble',  // Surface approvals to parent terminal
}
```

Fork children share the parent's prompt-cache prefix. `buildForkedMessages()` clones the full assistant message (thinking, text, all `tool_use` blocks) and builds a single user message with placeholder `tool_result`s for every `tool_use`. Only the final text block (the per-child directive) differs — byte-identical API prefixes maximize cache hits across forks.

**Path 2: Traditional teammates** (explicit spawn, separate terminal):
```typescript
type SpawnTeammateConfig = {
  name: string
  prompt: string
  cwd?: string
  use_splitpane?: boolean
  plan_mode_required?: boolean
  model?: string
  agent_type?: string
}
```

Backend detection: tmux pane, split-pane, or in-process fallback. Each teammate gets its own terminal with inherited env vars. File-based team communication via `readTeamFileAsync()` / `writeTeamFileAsync()`.

**Recursive protection:** `isInForkChild()` scans message history for `FORK_BOILERPLATE_TAG` to prevent fork children from forking again.

### pi-mono — No Subagent Spawning

**No multi-agent spawning mechanism.** Single agent processes tools in sequence or parallel (mode-dependent via `QueueMode`). Tool results stream back to a single conversation context. No background task spawning for child agents.

### hermes-agent — ThreadPoolExecutor Delegation

**`delegate_task` tool** spawns child agents in a thread pool:

```python
MAX_CONCURRENT_CHILDREN = 3
MAX_DEPTH = 2
```

Each child gets an isolated conversation (no parent history), a fresh `task_id`, and a restricted toolset — `delegate_task`, `clarify`, `memory`, `send_message`, and `execute_code` are all blocked.

**Batch mode:** Up to 3 tasks run in parallel via `ThreadPoolExecutor`. Parent blocks until all children complete.

**Result flow:** JSON summaries with tool trace metadata (tool names, arg/result bytes, exit reasons). Tool calls and reasoning from children are never visible in parent context.

**Progress display:** Tree-view lines above CLI spinner with emoji + tool names. Gateway mode batches tool names in 5-item increments.

**Depth limiting:** `_delegate_depth` tracks nesting. Children reject further delegation at depth >= 2 via `DELEGATE_BLOCKED_TOOLS` frozenset.

### claw42 — Async Task Spawning

**Tokio-based subagent registry:**

```rust
pub struct SubagentRegistry {
    agents: HashMap<String, SubagentHandle>,
}
pub struct SubagentHandle {
    pub id: String,
    pub task: String,
    pub status: SubagentStatus,  // Running | Completed | Failed(String)
    pub result: Option<String>,
    pub join_handle: Option<JoinHandle<()>>,
    pub depth: u8,
}
```

Results posted via ControlPlaneClient to parent session and broadcast to WebSocket connections. Result preview truncated at 500 chars.

**Heartbeat loop** spawned as a background tokio task — runs agent turns periodically (default 30min interval). Includes self-healing checks (Chrome CDP, workspace disk) before each agent turn. Uses `try_lock()` on shared `agent_turn_lock` — skips tick if already running.

### OpenAI Codex — Hierarchical Agent Control

**Full hierarchical spawning with depth limits and batch processing:**

```rust
session.services.agent_control.spawn_agent_with_metadata(
    config,
    input_items,
    thread_spawn_source(...),
    SpawnAgentOptions {
        fork_parent_spawn_call_id: ...,
        fork_mode: SpawnAgentForkMode::FullHistory,
    },
)
```

**Depth limiting:** `next_thread_spawn_depth()` tracks child depth. Returns error "Agent depth limit reached. Solve the task yourself."

**Batch/Job processing (Agent Job Tool):**
- `spawn_agents_on_csv` — one worker per CSV row
- `max_concurrency`: default 16, capped by config
- `max_runtime_seconds` per worker: default 1800s
- `output_schema` for structured result validation
- Workers report results via `report_agent_job_result(job_id, item_id, result)`
- Parent blocks until all complete; auto-exports to output CSV

**Fork mode:** `SpawnAgentForkMode::FullHistory` passes parent's full conversation. `fork_parent_spawn_call_id` tracks lineage.

### AnythingLLM — No Subagent Spawning

**No explicit parallel subagent spawning.** AIbitat framework chains skills sequentially within a single agent loop. Multiple agents can collaborate via channels and message passing, but not in a spawn-from-tool pattern.

### opencode — Role-Based, No True Background Agents

**No true background subagents.** Agent roles (`build`, `plan`, `general`, `explore`, `compaction`, `title`, `summary`) are behavioral modes, not separate processes. Roles switch via the `agent` field in messages. Hidden agents (`compaction`, `title`, `summary`) run as single-purpose LLM calls within the same process.

The distinction matters: opencode's "agents" are prompt configurations, not concurrent execution units. There is no spawn-from-tool, no thread pool, no background task queue.

---

## 6. Plan vs Build Mode

### Claude Code — First-Class Plan Mode

**Entry:** `/plan` slash command, or `EnterPlanModeTool` / `ExitPlanModeV2Tool` (formal tools).

**Tool restrictions:**
- Plan mode: read-only tools only (Bash restricted to safe commands, Glob, Grep, FileRead)
- Build mode: full tool access
- Gating: `prepareContextForPlanMode()` applies permission restrictions

**Plan artifact:** Markdown file at `~/.claude/plans/{slug}.md`. Slug generated via `generateWordSlug()`. Editable by user before approval. Separate plan files per subagent: `{slug}-agent-{agentId}.md`.

**Separate agent:** YES — `PLAN_AGENT` is a built-in read-only architecture specialist. Disallows `AgentTool`, `ExitPlanMode`, `FileEdit`, `FileWrite`.

**Multi-phase workflow:**
1. Read-only exploration
2. Design phase (plan file created)
3. User approval
4. Build phase (full tool access)

### pi-mono — Extension-Based Plan Mode

**Entry:** `/plan` command, `Ctrl+Alt+P`, or `--plan` CLI flag.

**Tool restrictions enforced via event hook:**
- Plan mode: `read`, `bash` (safe commands only via `isSafeCommand()`), `grep`, `find`, `ls`, `questionnaire`
- Build mode: `read`, `bash`, `edit`, `write`
- Pre-`tool_call` event hook blocks unsafe bash commands (rm, mv, cp, git write, npm install)

**Plan artifact:** Extracted from assistant message under "Plan:" header. Numbered todo items with `(step, text, completed)` fields. Persisted via `pi.appendEntry("plan-mode", {...})`.

**Execution tracking:** `[DONE:n]` markers in assistant response. Widget shows `☑ completed / ○ pending` progress. Agent iterates through remaining steps automatically.

### hermes-agent — Skill-Based Planning

**Entry:** `/plan [description]` slash command.

**No tool-level restrictions.** Plan enforced via skill description/prompt only ("do not implement code, do not edit project files except plan file").

**Plan artifact:** Markdown file under `.hermes/plans/YYYY-MM-DD_HHMMSS-<slug>.md` with structured sections: Goal, Context, Approach, Step-by-step, Files likely to change, Tests, Risks/tradeoffs.

**No formal transition:** Agent continues in same session after planning. Plan file is reference only, not enforced.

### claw42 — No Plan Mode

**No plan/build separation.** Workspace modes (shared vs isolated) are infrastructure-level container isolation, not agent-level workflow stages.

### OpenAI Codex — Collaboration Mode

**Entry:** Collaboration mode UI selection. Modes: `Plan`, `Default`, `Execute`, `Pair Programming`.

**Tool restrictions:**
- Plan mode: read-only (file reading, searching, git read, tests/builds that don't edit tracked files)
- `update_plan` tool explicitly rejected in Plan mode with error
- Build mode: full tool access

**Plan artifact:** `<proposed_plan>` XML block in assistant message. Client renders specially. Not persisted to disk automatically.

**Three phases (strict adherence):**
1. Explore & ground in environment
2. Intent chat (clarify requirements)
3. Implementation chat (decision-complete spec)

### AnythingLLM — No Plan Mode

**No plan/build separation.** Task-driven agentic loop with multi-agent delegation, not explore→design→build workflow.

### opencode — Dual-Agent Architecture

**Entry:** Agent field in messages: `agent: "plan"` vs `agent: "build"`. Tool: `plan_exit` to switch.

**Tool restrictions:** Plan mode disallows all edit tools via prompt + file write limited to plan file only.

**Plan artifact:** Markdown file at session-specific path from `Session.plan(session)`. Editable before approval.

**Separate agents:** YES — `plan` and `build` are distinct agents with separate system prompts (`PROMPT_PLAN` and `BUILD_SWITCH`). Build agent receives plan reference: "A plan file exists at... You should execute on the plan defined within it."

---

## 7. Skills Systems

### Claude Code — File-Based Discovery + Built-in Skills

**Definition:** `SKILL.md` files with YAML frontmatter (`name`, `description`, `when-to-use`, `allowed-tools`, `model`, `disable-model-invocation`, `hooks`).

**Discovery:**
- Root `.md` files in `~/.claude/skills/` and `.claude/skills/`
- Directories containing `SKILL.md` (recursive scan)
- Symlink-aware deduplication via `realpath()`

**Progressive disclosure:**
- Level 0: Only frontmatter (name, description) in system prompt via XML `<available_skills>` block. Descriptions truncated to 250 chars.
- Full content loaded on demand via `SkillTool` when model invokes `/skill:name`

**Built-in skills:** Bundled programmatically via `registerBundledSkill()`. Includes: debug, batch, skillify. Inlined into binary, extracted to disk on first invocation.

**Cross-agent sharing:** YES — `"skills": ["~/.claude/skills", "~/.codex/skills"]` in settings.

**Compaction:** Frontmatter survives (in system prompt). Full content loaded on demand, so it's ephemeral.

### pi-mono — Agent Skills Standard

**Implements [agentskills.io specification](https://agentskills.io/specification).**

**Discovery locations:**
- Global: `~/.pi/agent/skills/`, `~/.agents/skills/`
- Project: `.pi/skills/`, `.agents/skills/` (ancestor walk to git root)
- Packages: `skills/` in `package.json` or `pi.skills` field
- Settings: `skills` array in config
- CLI: `--skill <path>` (repeatable, additive with `--no-skills`)

**Three-level progressive disclosure:**
- Level 0: `skills_list()` → name + description in XML (~3k tokens)
- Level 1: `skill_view(name)` → full content + metadata
- Level 2: `skill_view(name, path)` → specific reference file

**Validation:** Name 1–64 chars, lowercase a-z/0-9/hyphens, must match parent directory.

**Name collisions:** First skill wins, diagnostic warning issued.

### hermes-agent — Hub-Based + Remote Registries

**Most advanced skills distribution system.** Three sources:

```
OptionalSkillSource  → official optional skills (not activated by default)
GitHubSource         → fetch via GitHub API (PAT, gh CLI, GitHub App auth)
WellKnownSource      → domain .well-known/skills/index.json endpoint
```

**Hub state management:**
- Lock file: `~/.hermes/skills/.hub/lock.json` (provenance tracking)
- Index cache: `~/.hermes/skills/.hub/index-cache/` (1-hour TTL)
- Quarantine: `~/.hermes/skills/.hub/quarantine/` (scanning)
- Audit log: `~/.hermes/skills/.hub/audit.log`
- Trust levels: `builtin` | `trusted` | `community`

**Conditional activation:**
- `fallback_for_toolsets`: skill hidden when toolset available, shown when missing
- `requires_toolsets`: skill hidden when toolset unavailable
- `platforms: [macos, linux]` — auto-hidden on non-matching OS

**Environment variables:** Declared in frontmatter via `required_environment_variables`. Hermes prompts securely only on local CLI (not messaging surfaces). Auto-passed to `execute_code` and `terminal` sandboxes.

**Installation:** `hermes skills install <identifier>` resolves short names. `hermes skills search [query]` for unified hub search.

### claw42 — Database-Backed Skills

**Not file-based.** Skills stored in PostgreSQL:

```sql
id, name, description, content, category, is_builtin, enabled, sort_order
```

**8 preset skills** seeded via `PRESET_SKILLS`: Test-Driven Development, Systematic Debugging, Verify Before Done, Code Review Mindset, Defensive Web Browsing, Structured Research, Incremental Delivery, Memory-Driven Learning.

**API-managed:** `GET /api/skills` (list), `POST /api/skills` (create/upsert). Agent-specific assignment via `GET /api/agents/[agentId]/skills`.

**No progressive disclosure.** Full content in system prompt as `SKILLS.md` section. No on-demand loading.

### OpenAI Codex — Rust-Based, Type-Safe

**TOML-based configuration** with `SkillsConfig` and `BundledSkillsConfig`. File-based `SKILL.md` discovery.

**Key types:**
- `SkillsManager` — orchestrates loading and execution
- `SkillMetadata`, `SkillPolicy`, `SkillDependencyInfo`
- `SkillScope` — defines access boundaries

**Dependency resolution:** Async resolution for skill environment variables. User input request for missing dependencies (session-only in-memory storage).

**Injection system:** `SkillInjections` + `build_skill_injections()` for explicit mention collection. `render_skills_section()` formats for system prompt.

**RPC protocol:** `SkillsListParams`, `SkillsListResponse`, `SkillsChangedNotification`, `SkillsConfigWriteParams` — full app-server protocol for skill management.

**Built-in samples:** `skill-creator`, `plugin-creator`, `skill-installer`, `openai-docs`.

### AnythingLLM — Hardcoded Skill Set

**No user-defined skills.** Fixed catalog in code: `rag-memory`, `document-summarizer`, `web-scraping`, `filesystem-agent`, `create-files-agent`, `create-chart`, `web-browsing`, `sql-agent`.

Each skill maps to a UI component. **Whitelist system:** `AgentSkillWhitelist` model gates per-user access. No file discovery, no progressive disclosure.

### opencode — Effect-Based Discovery + Remote Skills

**Distributed discovery:**
- External dirs: `.claude/`, `.agents/` (shared with other harnesses)
- Global: `~/.claude/skills/`, `~/.agents/skills/`
- Project: ancestor walk to worktree root
- Config: `skills.paths` explicit paths
- Remote URLs: `skills.urls` with `index.json` manifest downloads

**Remote skill fetching:**
- Fetches `index.json` from URL (schema-validated)
- Downloads skills matching manifest
- 8 concurrent download limit
- Local cache in `~/.opencode/cache/skills/`

**Permission integration:** Skills filtered by `Permission.evaluate("skill", skillName, agent.permission)` — allow, deny, or default.

**Effect-based architecture:** Lazy loading via Effect monad. Service-based dependency injection. `InstanceState` singleton for skill registry.

**Cross-agent sharing:** YES — loads from `.claude/` and `.agents/` directories.

---

## 8. Persona & Role Systems

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

## 9. Key Architectural Decisions

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

## 10. Implications for sidekar repl

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
