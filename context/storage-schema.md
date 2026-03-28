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
| `outbound_requests` | Request tracking for replies | No |
| `bus_queue` | Direct agent-to-agent messages | No |
| `error_events` | Append-only error log | No |
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

## Encryption

`kv_store` and `totp_secrets` are encrypted at rest using AES-256-GCM. The encryption key is derived from the device token and stored markers in `encryption_meta`.

System tables (`config`, `cron_jobs`, etc.) are unencrypted because:
- `config` bootstraps auth (chicken-and-egg)
- `cron_jobs` need to run before user authenticates
- `error_events` should be readable for debugging

## Migration notes

- Auth data was moved from separate `auth` table to `config` with `auth:` prefix (2026-03)
- Legacy `auth` table is auto-migrated on first run
