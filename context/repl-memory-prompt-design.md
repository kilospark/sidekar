# REPL Memory & System Prompt Design Discussion

Date: 2026-04-01

Cross-repo analysis of system prompts and memory mechanisms from pi-mono, free-code (Claude Code), hermes-agent, and anything-llm. Applied to sidekar repl design.

## System Prompt Architecture Comparison

| | **Claude Code** (free-code) | **pi-mono** | **hermes-agent** | **AnythingLLM** |
|---|---|---|---|---|
| **Structure** | Dynamic sections (18+), cached vs cache-breaking per turn | Layered builder (intro, tools, guidelines, context, skills, date) | 7-layer assembly (identity, tool guidance, honcho, memory, skills, context files, metadata) | Simple: base + context injection |
| **Context files** | CLAUDE.md from 4 tiers (managed, user, project, local) + ancestor walk + `@include` directives + `.claude/rules/*.md` glob-conditional | AGENTS.md/CLAUDE.md from cwd + ancestors + global | .hermes.md/HERMES.md (first wins) → AGENTS.md → CLAUDE.md → .cursorrules | Workspace settings only |
| **Skills** | XML `<available_skills>` with name/description/location; model reads full file on demand | Same XML format; `disableModelInvocation` flag | Markdown index with category groupings; conditional show (requires/fallback_for toolsets) | N/A |
| **Caching** | Prompt split at cache boundary marker; cached prefix reused across turns; cache-breaking sections recompute | System prompt rebuilt only on tool change or extension reload | Frozen snapshot — rebuilt only after compression | N/A |

## Memory Systems Comparison

| | **Claude Code** | **pi-mono** | **hermes-agent** | **AnythingLLM** |
|---|---|---|---|---|
| **Persistent memory** | 4-type taxonomy (user/feedback/project/reference) in `~/.claude/projects/<proj>/memory/` as markdown+frontmatter | None (compaction summaries only) | 2 files: MEMORY.md (2200 chars) + USER.md (1375 chars), § delimited entries | Vector DB plugin (explicit store/search) |
| **Session memory** | Structured template (9 sections: Title, State, Tasks, Files, Workflow, Errors, Docs, Learnings, Results, Worklog) — 12K token cap | Compaction entries in JSONL session file | FTS5 search across all sessions + Honcho dialectic user modeling | Conversation history (last N messages) |
| **Auto-extraction** | Forked subagent (max 5 turns) runs after each query, fire-and-forget, mutually exclusive with main agent writes | None — manual compaction only | None — agent writes memory explicitly via tool | None |
| **Memory recall** | Sonnet side-query selects up to 5 relevant topic files from manifest; MEMORY.md always in system prompt | buildSessionContext() injects compaction summaries as messages | Frozen snapshot injected at session start; live writes don't change prompt until compression | Vector similarity search (top-K) |
| **Index** | MEMORY.md (200 lines, 25KB max) — one-line pointers to topic files | N/A | N/A (entries are inline in MEMORY.md/USER.md) | N/A |

## Key Design Patterns

### 1. Frozen snapshot + compression-triggered reload (hermes)
System prompt is cached for prefix cache stability. Memory writes during a session don't change the prompt. Only after compaction does the memory reload, capturing new writes.

### 2. Four-type memory taxonomy (Claude Code)
user/feedback/project/reference with explicit "what NOT to save" rules prevents memory bloat. The `description` field enables relevance-based recall without reading full files.

### 3. Auto-extraction via forked subagent (Claude Code)
Fire-and-forget background extraction after each turn. Max 5 turns. Read-only tools except for memory dir. Cursor-based (only processes new messages). Coalescing prevents duplicate runs.

### 4. Session search with FTS5 + LLM summarization (hermes)
Raw FTS5 is fast; expensive LLM summarization only on top N results. Empty query = instant recent sessions list (zero LLM cost).

### 5. Structured session notes (Claude Code)
9-section template (Current State, Task spec, Files, Workflow, Errors, Learnings, etc.) with 2000 tokens/section cap. Preserved through compaction.

### 6. Honcho dialectic modeling (hermes)
External service that builds a user model from conversation history. Prefetched in background, consumed next turn.

## Adoption Priority for sidekar repl

| # | Feature | From | Effort | Why |
|---|---------|------|--------|-----|
| 1 | Frozen system prompt with compression reload | hermes | Small | Cache stability |
| 2 | Memory tool (store/recall/forget) | hermes | Medium | Simple 2-file approach |
| 3 | FTS5 on session entries | hermes | Small | Already have SQLite |
| 4 | Context file hierarchy | Claude Code | Small | Add .sidekar/rules/*.md |
| 5 | Structured compaction summary | pi-mono/CC | Done | Already implemented |
| 6 | Auto-memory extraction | Claude Code | Large | Needs careful CLI design |

## Open Design Questions

### SKILL.md in system prompt

**Current:** sidekar's SKILL.md (the full CLI reference doc, ~4000 tokens) is injected directly into the system prompt.

**Problem:** In REPL mode, the LLM IS sidekar — it calls sidekar through the bash tool. The SKILL.md was written for OTHER agents (Claude Code, Codex, Cursor) to learn how to use sidekar. But when sidekar is the agent, it already has bash/read/write/edit/glob/grep as native tools. Injecting SKILL.md:
- Wastes ~4000 tokens of context on every turn
- Contains instructions like "Use the CLI help as the source of truth" — but the REPL already IS the CLI
- Has operating rules and targeting priority that are redundant with the built-in tool descriptions

**Options:**
1. **Don't inject SKILL.md** — the LLM already has sidekar commands available via bash. If it needs to know a command, it can run `sidekar help <command>`.
2. **Inject a slim summary** — a short (~500 token) capability map listing command categories without full syntax. The LLM runs `sidekar help <command>` for details.
3. **Inject only non-obvious commands** — skip browser/desktop basics, include bus, memory, tasks, compact, and other coordination commands that the LLM wouldn't discover on its own.
4. **Progressive disclosure like pi-mono** — list skills as name+description in prompt, LLM reads the full file only when needed. This is what pi-mono does with skills.

**Recommendation (TBD):** Option 2 or 4 — slim capability map in prompt, full details on demand via `sidekar help` or `read SKILL.md`.

### Memory approach

sidekar already has `sidekar memory` — a full FTS5-backed memory system with types, tags, search, compact, and patterns. The REPL could expose this as a tool directly rather than building a separate memory system. The LLM would call `sidekar memory write "fact"` and `sidekar memory search "topic"` through bash.

### Context files

sidekar currently loads AGENTS.md and CLAUDE.md from cwd + ancestors. Should also support:
- `.sidekar/rules/*.md` (project-specific REPL instructions)
- `~/.sidekar/SYSTEM.md` (global REPL personality)
- `SIDEKAR.md` (project-level sidekar-specific instructions)

### Session search

The `repl_entries` table could get an FTS5 virtual table for cross-session search, similar to hermes. Combined with sidekar's existing memory search, this would give the REPL agent full recall across both structured memory and conversation history.
