# Cross-account secrets

## Goal

Linked Sidekar accounts should be able to list and use shared REPL provider
credentials, KV entries, and TOTP entries the same way they can list and attach
to active relay sessions.

This must not make all linked accounts secret peers by default. Active relay
sessions and secrets have different risk profiles.

## Current state

- Account links live in MongoDB `account_links`.
- A directed link is `grantor_id -> grantee_id`.
- The grantee can currently aggregate grantor resources for:
  - active relay sessions
  - relay terminal attach/input
  - device listing
- Local secrets live in SQLite on the device:
  - REPL provider credentials: `kv_store` keys with `oauth:` prefix
  - KV secrets: `kv_store`
  - TOTP secrets: `totp_secrets`
- Local secret APIs are scoped by `broker::current_user_id()`, but REPL
  credential commands currently run through the REPL top-level dispatch before
  generic encryption/user bootstrap.

## Non-goals

- Do not copy all local SQLite secret rows into MongoDB as plaintext or
  decryptable server-side payloads.
- Do not grant secret access from the existing session/device link alone.
- Do not expose remote TOTP seed material by default.
- Do not make linked grantees able to delete or mutate grantor secrets in v1.

## Grant model

Add explicit scopes to linked-account grants. Existing links should keep their
current behavior by default:

- `sessions`
- `devices`
- `credentials`
- `kv`
- `totp`

For backward compatibility, a missing scope array means `sessions` and
`devices` only. Secret scopes must be granted explicitly.

## Reference model

All list and use surfaces should accept a single reference syntax:

- Local credential: `codex`
- Remote credential: `owner/codex`
- Local KV: `OPENAI_API_KEY`
- Remote KV: `owner/OPENAI_API_KEY`
- Local TOTP: `github user@example.com`
- Remote TOTP: `owner/github user@example.com`

`owner` should resolve against linked account login/name metadata. Internally,
references should normalize to:

```text
SecretRef {
  owner: Local | Remote { user_id, label },
  kind: Credential | Kv | Totp,
  name/service/account
}
```

## Transport model

Use remote owner daemon RPC for v1.

Reason: the authoritative secret material is on the owner device in local
SQLite, often encrypted with the owner account key. A grantee can only use a
remote secret while an owner device capable of serving secret RPCs is online.

Server-side encrypted sync can be added later, but it needs key wrapping and a
separate revocation story.

## RPC shape

Relay/server verifies grant scope, then forwards a request to an online owner
daemon/tunnel.

Operations:

- `secret_list { kind }`
- `credential_resolve { name }`
- `kv_get { key }`
- `kv_get_for_exec { keys, tag }`
- `totp_code { service, account }`
- `totp_show { service, account }` only if a future `totp:read_secret` grant is added

List RPC returns metadata only:

- origin owner label
- stable owner user id
- secret kind
- display ref
- local name/key/service/account
- provider label/email for credentials
- tags for KV
- algorithm/digits/period for TOTP
- capability flags: `can_use`, `can_read`, `can_write`

Use RPC returns the minimum required secret value:

- REPL credential: provider access token or provider config needed to build a provider
- KV get/exec: KV value
- TOTP get: current TOTP code

## User surfaces

Every listing should merge local and granted remote entries.

- `sidekar repl credentials`
- `/credential list`
- `/credential` picker
- `sidekar kv list`
- `sidekar kv get`
- `sidekar kv exec`
- `sidekar totp list`
- `sidekar totp get`

Mutating operations stay local in v1:

- credential add/update/delete
- kv set/delete/tag/history/rollback
- totp add/remove/show/qr

If a remote ref is passed to a local-only mutation, fail with an explicit error.

## Security requirements

- Secret grants are explicit and scoped.
- Every remote secret use is audited on the owner side and server side.
- TOTP `get` returns a code, not the seed.
- Credential refresh happens on the owner side when possible.
- Remote credential provider objects should not persist remote refresh tokens
  into the grantee local KV.
- Remote KV values must be masked in `kv exec` output the same way local values
  are masked.
- Offline owner device should produce a clear unavailable error.

## Implementation phases

1. Add local-only secret facade and `SecretRef` parser. Route list/use call
   sites through this facade without behavior changes.
2. Add account-link scopes and settings UI/API for secret grants.
3. Add owner daemon/relay RPC for metadata list. Merge local and remote list
   output.
4. Add remote `kv get`, `kv exec`, and `totp get`.
5. Add remote REPL credential resolution and provider construction.
6. Add audit logs, denial/offline tests, and end-to-end linked-account tests.

## Implemented v1

- Account links carry explicit scopes. Missing `scopes` preserves legacy
  `sessions` + `devices` only.
- Settings invite flow lets grantors choose `sessions`, `devices`,
  `credentials`, `kv`, and `totp`.
- Relay exposes `/relay/secrets`, validates device auth + grant scope, and
  forwards requests to online owner tunnels. Cross-relay owner origins are
  forwarded once.
- Owner tunnels answer `secret_request` frames from local SQLite-backed
  credentials/KV/TOTP.
- CLI/REPL list and use paths merge local rows with granted remote rows:
  credentials, KV list/get/exec, TOTP list/get, and REPL provider construction.
- Remote TOTP returns only current code. Remote credential refresh runs on the
  owner side.

Known v1 limits:

- Owner device must be online with a multiplex relay tunnel.
- Mutating operations remain local-only.
- Audit logging is not yet implemented beyond existing broker/relay logging.

## Tests

- Local-only facade returns same credentials/KV/TOTP rows as current broker APIs.
- `owner/name` parses as remote and `name` parses as local.
- Remote refs are rejected by local-only mutation paths.
- Grant without `credentials` does not list credentials.
- Grant with `credentials` lists remote credentials but does not allow delete.
- Grant with `totp` returns current code but not raw seed.
- Offline owner returns unavailable, not not-found.
