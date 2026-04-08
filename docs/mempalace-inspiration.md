---
name: mempalace inspiration for sidekar
description: Analysis of ~/src/mempalace patterns and abstractions that could improve sidekar's memory, bus, and agent systems
type: project
---

# MemPalace Inspiration for Sidekar

Source: `/Users/karthik/src/mempalace` (Python, MIT license, local-first AI memory system)

**Why:** mempalace achieves 96.6% on LongMemEval benchmark with zero API calls. Its patterns for memory persistence, temporal facts, and agent coordination are directly applicable to sidekar's SQLite-based infrastructure.

**How to apply:** Cherry-pick the high-value patterns below into sidekar's existing `memory` and `bus` subsystems. No new dependencies needed — everything maps to SQLite.

---

## High-Value Patterns

### 1. Temporal Knowledge Graph
mempalace stores facts as triples with validity windows:
```
(subject, predicate, object, valid_from, valid_to, confidence)
```
Example: `("site-x", "requires-login", "true", "2026-01-01", "2026-03-15")`

**For sidekar:** Agent observations decay. A fact learned during a scrape session ("yahoo finance layout changed") should have a timestamp so future sessions know how stale it is. Sidekar's `memory` already uses SQLite — add a triples table with `valid_from`/`valid_to` columns. Query with `as_of` parameter to see facts true at a point in time.

**Key files:** `mempalace/knowledge_graph.py` — SQLite schema, `add_triple()`, time-filtered queries, invalidation

### 2. Agent Diaries (Per-Agent Persistent Journal)
Each agent gets an isolated wing for journaling:
- Write timestamped, topic-tagged entries
- Read back history: `diary_read(agent_name, last_n=10)`
- Agents can read each other's diaries by name

**For sidekar:** Bus messages are ephemeral — they disappear after delivery. Agent diaries would let agents learn from prior sessions' outcomes. A scraper agent could write "yahoo-finance after-hours endpoint moved to /quote/{symbol}" and a future session would find it via search.

**Key files:** `mempalace/mcp_server.py` lines 349-392 (tool_diary_write/read)

### 3. Exchange-Pair Chunking
Conversations are chunked by (user question + AI response) pairs, not line count:
- Falls back to paragraph chunking if no exchange markers
- Falls back to line-groups (25 lines) if no paragraphs

**For sidekar:** `compact` could use this for agent transcripts — keep the action+result together as an atomic unit instead of arbitrary line splits.

**Key files:** `mempalace/convo_miner.py` lines 68-101

### 4. Duplicate Detection Before Storage
Semantic similarity check (top 5 nearest neighbors) before adding new content. Returns `is_duplicate` bool + list of similar matches.

**For sidekar:** `memory write` could check for near-duplicates before filing. Prevents the same observation from piling up across sessions.

**Key files:** `mempalace/mcp_server.py` lines 183-215 (tool_check_duplicate)

### 5. 4-Layer Memory Stack (Lazy Loading)
```
L0: Identity     (~50 tokens)   — always loaded
L1: Essential    (~120 tokens)  — auto-generated summary
L2: On-Demand    (~200-500)     — wing-specific, loaded when needed
L3: Deep Search  (unlimited)    — full semantic search
```
Wake-up costs only ~170 tokens (L0+L1), leaving 95%+ context free.

**For sidekar:** Bus agents could adopt this for context-efficient session bootstrapping. Instead of loading all memory at start, load identity + last session summary, then search deeper on demand.

**Key files:** `mempalace/layers.py`

---

## Lower Priority / Interesting

### AAAK Lossy Compression Dialect
Structured symbolic format for compressing long content:
```
Header:  FILE_NUM|PRIMARY_ENTITY|DATE|TITLE
Zettel:  ZID:ENTITIES|topic_keywords|"key_quote"|WEIGHT|EMOTIONS|FLAGS
```
3-letter entity codes, emotion markers, flags (ORIGIN, CORE, DECISION, TECHNICAL). Readable by any LLM without decoder.

**For sidekar:** Could compress batch job progress logs: `JOB:42|task.scrape|✓.10of20|runtime:5m42s`

**Key files:** `mempalace/dialect.py`

### Signal-Based Entity Detection (No API)
Pattern-based classification using verb patterns, pronouns, dialogue markers:
```python
PERSON_VERB_PATTERNS = [r"\b{name}\s+said\b", r"\b{name}\s+asked\b", ...]
PROJECT_VERB_PATTERNS = [r"\bbuilding\s+{name}\b", r"\bdeployed?\s+{name}\b", ...]
```

**For sidekar:** Auto-categorize agent work (browser-action, api-call, file-modification) without LLM calls.

**Key files:** `mempalace/entity_detector.py` lines 24-89

### Auto-Save Hooks with State Machine
Stop hook uses `stop_hook_active` flag as toggle to prevent infinite loops:
- Block once → AI saves → next call sees flag → let through

**For sidekar:** Checkpoint agent state at key moments without interrupting flow.

**Key files:** `mempalace/hooks/mempal_save_hook.sh` lines 75-106

---

## Architecture Notes

- **Storage:** ChromaDB (vector store) + SQLite (knowledge graph) — all local, no cloud
- **No summarization:** Stores verbatim text, never summarizes away context
- **MCP server:** 19 tools via JSON-RPC, dict-based tool registry with schema + function pointers
- **Config priority:** Env vars > config file > defaults
- **Format normalization:** Handles Claude Code JSONL, Codex JSONL, Claude.ai JSON, ChatGPT JSON, Slack JSON
