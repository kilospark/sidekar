# Bus Request Lifecycle

## Goal

Upgrade Sidekar's bus from a loose send/reply model into a first-class async
request lifecycle:

- durable outbound request state
- durable stored replies
- explicit request status transitions
- CLI inspection for open requests and received replies

This is intentionally asynchronous. Sidekar should not add synchronous RPC for
agent-to-agent messaging.

## Current State

Today the local broker uses three tables:

- `bus_queue` for delivery
- `pending_requests` for recipient-side request tracking
- `outbound_requests` for sender-side nudge/timeout tracking

The main limitations are:

- replies clear tracking by deleting rows instead of transitioning state
- there is no durable reply history
- there is no CLI for inspecting request lifecycle
- request state is optimized for nudges, not for visibility

## Phase 1

Phase 1 is the local async request/reply upgrade.

### Storage

Extend `outbound_requests` with:

- `kind`
- `channel`
- `project`
- `message_preview`
- `status`
- `answered_at`
- `timed_out_at`
- `closed_at`

Add `bus_replies`:

- `reply_to_msg_id`
- `reply_msg_id`
- `sender_name`
- `sender_label`
- `kind`
- `message`
- `created_at`
- `envelope_json`

### Status model

Valid outbound request statuses:

- `open`
- `answered`
- `timed_out`
- `cancelled`

State should be preserved by status transition, not erased during normal reply
or timeout handling.

### CLI

Add read-only inspection commands:

- `sidekar bus requests`
- `sidekar bus replies`
- `sidekar bus show <msg_id>`

These are inspection commands only. They do not change transport behavior.

### Runtime boundary

Phase 1 started local-broker-first.

The current transport boundary is:

- local broker delivery stores request lifecycle directly in SQLite
- relay delivery still pastes the human-readable body into the remote PTY
- relay now also carries optional structured `Envelope` JSON alongside that
  pasted body for multiplex tunnel sessions

That means:

- local same-machine workflows have full durable request/reply tracking
- cross-machine relay workflows can now persist remote pending requests and
  sender-side reply history when both sides are connected through multiplex
  relay tunnels
- older or plain-text-only relay paths still degrade to pasted-body delivery

## Phase 2

Phase 2 adds durable local session metadata.

### Goal

Keep a durable local record of Sidekar agent sessions separate from the live
`agents` registry.

`agents` remains the presence table for currently registered sessions.
`agent_sessions` becomes the historical log and metadata layer for:

- recent local agent sessions
- per-session request/reply counters
- recent activity inspection
- future naming, notes, and lightweight continuity features

This is local Sidekar session metadata, not remote account sessions and not a
chat-platform session model.

### Storage

Add `agent_sessions`:

- `id`
- `agent_name`
- `agent_type`
- `nick`
- `project`
- `channel`
- `cwd`
- `started_at`
- `ended_at`
- `last_active_at`
- `request_count`
- `reply_count`
- `message_count`
- `last_request_msg_id`
- `last_reply_msg_id`

Suggested SQL shape:

```sql
CREATE TABLE agent_sessions (
  id TEXT PRIMARY KEY,
  agent_name TEXT NOT NULL,
  agent_type TEXT,
  nick TEXT,
  project TEXT,
  channel TEXT,
  cwd TEXT,
  started_at INTEGER NOT NULL,
  ended_at INTEGER,
  last_active_at INTEGER NOT NULL,
  request_count INTEGER NOT NULL DEFAULT 0,
  reply_count INTEGER NOT NULL DEFAULT 0,
  message_count INTEGER NOT NULL DEFAULT 0,
  last_request_msg_id TEXT,
  last_reply_msg_id TEXT
);

CREATE INDEX idx_agent_sessions_agent_name
  ON agent_sessions(agent_name, started_at DESC);

CREATE INDEX idx_agent_sessions_project
  ON agent_sessions(project, started_at DESC);

CREATE INDEX idx_agent_sessions_last_active
  ON agent_sessions(last_active_at DESC);
```

This should complement the live `agents` registry, not replace it.

### Session identity

Use a Sidekar-owned durable session id created at PTY launch.

Recommended shape:

- `pty:<pid>:<started_at>`

or a random generated id if we want to avoid pid reuse concerns.

Do not use the bus agent name as the primary key. Agent names are useful, but
they are not a durable session identity.

### Lifecycle

On PTY start:

- create one `agent_sessions` row
- populate:
  - `agent_name`
  - `agent_type`
  - `nick`
  - `project`
  - `channel`
  - `cwd`
  - `started_at`
  - `last_active_at`

On outbound request send:

- increment `request_count`
- increment `message_count`
- set `last_request_msg_id`
- update `last_active_at`

On locally recorded reply:

- increment `reply_count`
- increment `message_count`
- set `last_reply_msg_id`
- update `last_active_at`

On PTY shutdown:

- set `ended_at`
- leave the row in place for inspection/history

### Runtime wiring

Recommended wiring points:

- `src/pty.rs`
  - create session row on PTY launch
  - mark ended on PTY shutdown
- `src/bus.rs`
  - update request counters on request/handoff send
  - update reply counters when a reply is recorded locally

Do not try to infer session history from the live `agents` row after the fact.
The point of Phase 2 is to make session history durable at write time.

### CLI

Add a separate namespace first instead of overloading authenticated account
sessions:

- `sidekar agent-sessions`
- `sidekar agent-sessions show <id>`

Suggested first output for `list`:

- session id
- agent name
- nick
- project
- channel
- started at
- last active at
- ended at
- request count
- reply count

Suggested `show` output:

- all list fields
- cwd
- last request msg id
- last reply msg id

### Non-goals

Phase 2 should not:

- replace `agents`
- change delivery transport
- implement synchronous request waiting
- become a generic conversation history system
- store full PTY transcripts

### Follow-on features

Once `agent_sessions` exists, the natural next features are:

1. session naming or notes
2. recent-session inspection in the dashboard
3. startup continuity hints based on recent session metadata
4. linking `agent_sessions` to memory session summaries

## Relay structured replies

Structured relay persistence now works by carrying an optional `envelope_json`
field through the relay bus path.

Flow:

1. sender creates a normal `Envelope`
2. relay HTTP stores:
   - pasted `body`
   - optional `envelope_json`
3. relay dispatcher forwards both to the recipient tunnel
4. recipient Sidekar:
   - enqueues the pasted body for the PTY
   - uses `envelope_json` to update local broker state
5. when the remote side replies, the sender's Sidekar receives the reply
   envelope and records it into `bus_replies`

Important rule:

- local recipient machines should only clear their pending row for a remote
  reply
- only the sender's machine should store the durable reply history for that
  request

### Implementation order

1. schema + broker helpers for `agent_sessions`
2. PTY start/end writes
3. bus counter updates
4. `sidekar agent-sessions`
5. `sidekar agent-sessions show <id>`

## Implementation Order

1. Add Phase 1 schema and broker helpers
2. Record replies and preserve timeout history
3. Add `bus requests`, `bus replies`, and `bus show`
4. Add Phase 2 `agent_sessions`
