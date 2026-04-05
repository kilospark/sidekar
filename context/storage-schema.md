# Storage Schema

All persistent state lives in `~/.sidekar/sidekar.sqlite3`.

## Tables

| Table | Purpose | Encrypted |
|-------|---------|-----------|
| `config` | System settings and auth tokens | No |
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

## Config table namespaces

The `config` table uses key prefixes for different categories:

- `auth:*` - Authentication data (device token, created_at)
- No prefix - User-configurable settings (telemetry, browser, etc.)

Example keys:
- `auth:token` - Device token from `sidekar login`
- `auth:created_at` - When device token was issued
- `telemetry` - Whether to send anonymous usage counts
- `browser` - Preferred browser for CDP sessions

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

System tables (`config`, `cron_jobs`, etc.) are unencrypted because:
- `config` bootstraps auth (chicken-and-egg)
- `cron_jobs` need to run before user authenticates
- `events` should be readable for debugging
