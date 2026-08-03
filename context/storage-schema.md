# Storage Schema

All persistent state lives in `~/.sidekar/sidekar.sqlite3`.

## Tables

| Table | Purpose | Encrypted |
|-------|---------|-----------|
| `config` | System settings and auth tokens | No |
| `prompts` | Agent prompt texts, editable via `sidekar prompt` | No |
| `kv_store` | User key-value storage | Yes |
| `totp_secrets` | TOTP authentication secrets | Yes |
| `cron_jobs` | Scheduled tasks | No |
| `agents` | Registered agents on the bus | No |
| `pending_requests` | Recipient-side request tracking | No |
| `outbound_requests` | Sender-side request lifecycle and nudge state | No |
| `bus_replies` | Durable stored replies for local request history | No |
| `agent_sessions` | Durable local Sidekar agent session metadata | No |
| `bus_queue` | Direct agent-to-agent messages | No |
| `events` | Append-only event log | No |
| `encryption_meta` | Encryption key markers | No |
| `repl_sessions` | REPL session metadata | No |
| `repl_entries` | REPL message history and non-message entries | No |
| `repl_input_history` | Project-scoped REPL line-edit history | No |

## Config table namespaces

The `config` table uses key prefixes for different categories:

- `auth:*` - Authentication data (device token, created_at)
- No prefix - User-configurable settings (telemetry, browser, etc.)

Example keys:
- `auth:token` - Device token from `sidekar device login`
- `auth:created_at` - When device token was issued
- `telemetry` - Whether to send anonymous usage counts
- `browser` - Preferred browser for CDP sessions

## Prompts table

Every prompt an agent sees ships as a default in `src/prompts/` and is seeded
into `prompts` on first read. Callers go through `crate::prompts::get(key)`,
which falls back to the compiled default when the row is missing, blank, or the
database will not open.

`edited = 1` marks a row the user changed, through `sidekar prompt set|edit` or
the daemon admin UI. Seeding refreshes only unedited rows, so an update never
overwrites a customization. `default_hash` records the default the row came
from; when it no longer matches the shipped default, an edited row is reported
as drifted so the user can compare and decide.

Reseeding is keyed off a hash of all defaults, stored in `config` under
`prompts:builtins_hash`, rather than `SCHEMA_VERSION`. Prompt wording changes
far more often than the schema, and tying the two together would mean every
prompt tweak needs a schema bump to reach existing installs.

## REPL tables

- `repl_sessions` stores session id, cwd, model, provider label, optional name, and timestamps.
- `repl_entries` stores persisted REPL history. `entry_type = 'message'` is the chat transcript; other entry types are reserved for non-message session metadata.
- `repl_input_history` stores submitted mini-line input history, scoped by canonical project root so up/down history survives REPL restarts for the same project.

## Bus table triad

Four tables work together for agent-to-agent messaging:

| Table | Role | Payload |
|-------|------|---------|
| `bus_queue` | Delivery | Plain text to paste into PTY |
| `pending_requests` | Recipient tracking | Full `Envelope` (awaiting reply) |
| `outbound_requests` | Sender lifecycle | Metadata + status for sent requests |
| `bus_replies` | Local reply history | Full reply envelope JSON |

These are **not duplicates**:
- `bus_queue` is the transport pipe (read-and-delete delivery)
- `pending_requests` tracks recipient-side unanswered requests
- `outbound_requests` tracks sender-side request lifecycle and timeouts
- `bus_replies` stores durable replies when the responding Sidekar process shares the same local broker DB

## Bus lifecycle notes

`outbound_requests.status` currently uses:

- `open`
- `answered`
- `timed_out`
- `cancelled`

This is local-broker-first. Cross-machine relay delivery still pastes plain text,
so durable reply storage is guaranteed today only when both sides share the same
local SQLite broker.

## Encryption

`kv_store` and `totp_secrets` are encrypted at rest using AES-256-GCM. The encryption key is derived from the device token and stored markers in `encryption_meta`.

`prompts` is deliberately its own table rather than a `config` namespace: the
daemon admin UI can write prompts, and a generic web-writable path into `config`
would expose `auth:token` to the same route.

System tables (`config`, `cron_jobs`, etc.) are unencrypted because:
- `config` bootstraps auth (chicken-and-egg)
- `cron_jobs` need to run before user authenticates
- `events` should be readable for debugging
