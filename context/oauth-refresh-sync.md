# OAuth refresh across machines, and keys that stay on one device

## Provider logins (`oauth:*`)

Provider credentials are kv keys, so they sync. Anthropic, Codex and Grok
rotate the refresh token on every refresh. Before this change, two machines
whose access token expired at the same time each refreshed with the same
refresh token. The provider honoured one; the other got `invalid_grant`, or the
provider revoked the whole token family, and that user was told to log in again.
Whichever save synced last won, so even the winner could end up overwritten
with a spent token.

`providers::oauth::refresh_sync::refresh_shared` now handles every refresh,
both on expiry (`get_token`) and after a 401 (`force_refresh_token`):

1. **Local lock.** A file lock per credential under `~/.sidekar/locks`. Processes
   on one machine refresh one at a time, and the second one reuses the first
   one's token.
2. **Adopt first.** It pulls the secrets channel. If another machine already
   refreshed, it uses that token. A token counts as usable when it isn't
   about to expire and isn't the one the provider just turned down.
3. **Lease.** While refreshing, the machine holds an account-wide lease named
   after the credential (`broker::claim_lease`). A machine refused the lease
   doesn't refresh: it pulls every 2s until the holder's token arrives. A
   holder that never delivers is passed over after the lease runs out
   (2 slots + 10s).
4. **Push now.** The new token is saved and pushed right away, so the others
   find it.
5. **Retry with newer.** If the refresh fails, the machine pulls again. If a
   different refresh token arrived, it uses that token's access token when
   still valid, or refreshes once with it. Only then is the error shown, along
   with the hint to log in again.

### The lease

The lease runs on the bus channel as kind `lease`. It needs no new endpoint,
and the records expire with that collection's TTL. The server's
compare-and-swap accepts a record only if its version is above the one it
holds. So the version is a slot of wall-clock time,
`now / LEASE_SLOT_SECS` (20s): one machine wins a slot, and any other is
refused it. The winner claims the next slot at once, so it holds the lease
for at least one full slot. This machine's own claims are kept under
`internal:lease:<id>`, so it can claim again while holding the lease.

This assumes machines' clocks agree to within a slot, which NTP gives. A clock
far ahead claims a slot far ahead, and the server then refuses every slot up to
it, for as long as the skew (or until the bus collection's TTL drops the
record). So a lease more than `LEASE_MAX_AHEAD_SLOTS` (3) slots past this
machine's own slot is not taken for a live holder's: the claim reads as
"unavailable" and the refresh goes ahead, with the old race as the worst case,
which step 5 recovers from. Without that cut-off every other machine would wait
out the full lease wait on each refresh. A hold this machine recorded that far
ahead (its own clock ran fast) is likewise ignored. A server from before leases answers 400. That reads as
"unavailable", and the refresh goes ahead without a lease, still with steps
1, 2, 4 and 5.

Releases 4.5.45 and earlier that pull the bus channel log a skipped record
for each lease. That's harmless.

## Per-device kv keys

Keys under `internal:` never sync (`kv_store::kv_key_syncs`). Two more kinds
of state that are per-device by nature moved there:

- **`_nick:<project>` → `internal:nick:<project>`.** A project's bus nickname.
  Synced, the same project on two machines got the same nick. Since bus sync,
  that makes `bus send <nick>` ambiguous.
- **`gemini_cache:<fp>` → `internal:gemini_cache:<fp>`.** Handles to
  short-lived Gemini context caches. They change often and aren't worth
  syncing.

`broker::migrate_device_local_kv_keys` runs on open, guarded by a read:

- It renames local rows. A value already set under the new key wins.
- It gives every synced copy a tombstone to push, so the server copy goes.
- It drops the clean tombstone rows once they're pushed.

Pulled records under the old prefixes are ignored, so an older build that
still writes them can't bring them back onto a newer one.

## Release order

The server change (`lease` added to the bus channel's kinds in
`www/api/v1/sync/secrets.js`) can ship before or after the binary.
Until it's live, refreshes run without a lease.
