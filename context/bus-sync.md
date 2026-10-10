# Bus sync: the bus across an account's machines

kv, totp, hotp and memory already sync between the machines on one account.
The bus did not. An agent registered on one machine was invisible to the
others, and a message could reach another machine only when its recipient
had a live relay tunnel (`--relay`). Bus sync carries both agent presence and
messages over the same mechanism the secrets use.

## What is reused

- **Records.** Encrypted (`$sync1$`, the account key), versioned, pushed with
  compare-and-swap, pulled from a watermark, paged. `sync_state` tracks them
  with `mark_dirty`, like a kv key.
- **Endpoint.** `/api/v1/sync/secrets?channel=bus`, in collection `bus_sync`.
  It is the same function because Vercel Hobby allows 12 and this deployment
  has 12. Its own collection means the frequent bus pulls never read the
  secret store, and a sidekar from before bus sync never receives these
  records. Records expire a week after their last change (TTL index): a
  delivered message is tombstoned, and presence is republished, so nothing
  older is worth keeping.
- **Watermark.** `sync_meta.last_bus_pull_at`, apart from the secrets one.
  A bus pull starts 60 seconds behind it. The watermark is one server
  instance's clock and a record's `updated_at` another's, stamped before its
  write commits, so skew or a late commit can leave a record at or below the
  watermark. kv heals on its next edit; a message is written once and would
  be lost. Re-pulling the overlap is harmless: messages are claimed once and
  agents are version-guarded.

## Presence (`agent` records)

Record id `<device id>\0<agent name>`. Each machine publishes its long-lived
registrations (`pty-`, `session-`, `repl-`, `mcp-` panes). A `cli-` pane is
one command and is never published. The daemon reconciles every round
(`bus_sync::reconcile_presence`): a new agent is published, each is
republished every 2 minutes, and one that left is tombstoned. Reconciling
from the `agents` table, rather than hooking register/unregister, also covers
agents removed by the dead-agent sweep and by older binaries.

Other machines keep these in `remote_agents`. One counts as present until 5
minutes pass without a heartbeat, so a machine that sleeps or crashes drops
out without anyone tombstoning its agents. A `published_at` ahead of the
receiver's clock is taken as now, so a fast sender clock cannot keep its
agents listed. `name@host` picks one machine; machines that share a host name
take a device-id prefix instead (`name@<first 8 chars>`, offered in the error).

## Messages (`bus` records)

`bus send` resolves a name in this order: this channel, this machine, a
relay session, then another machine's published agent. The last queues a
`bus` record (id: the message id) addressed to that agent's device in
`bus_outbox`, and pushes it at once. The push failing leaves it queued;
the daemon's next round retries.

Several processes pull this channel on one machine (the daemon's round, `bus
await`, `bus send`, `bus who --all`), so delivery starts with a claim: one
write transaction inserts the record's `sync_state` row, already tombstoned
for push, and only the caller whose insert created it delivers. A delivery
that fails releases the claim for a later pull.

The claimant delivers it as the relay delivers a tunnelled message:
a request is set pending, an answer is recorded against its request, and the
text goes into the local queue (only if the recipient is registered there;
otherwise it would reach the next agent to take the name). It then
tombstones the record.

A request's origin device is kept in `bus_remote_origin`. An answer whose
addressee is no longer anywhere (a one-shot `bus send` from a shell) goes
back to that device, where it is recorded for `bus await`.

## Timing and cost

The daemon runs a round every `bus_sync_interval_secs` (default 15, 0 turns
bus sync off): reconcile, pull if this machine has an agent another could
message, push what is pending. Each round is one or two requests to
sidekar.dev, so a machine with no agents only pushes presence changes.
Callers that cannot wait pull for themselves: `bus await` every 3 seconds
while its request went to another machine, `bus who --all` before listing,
and `bus send` once when a name is not found. `sidekar _bus_sync` runs one
round by hand.

Delivery between machines is therefore up to one interval late, where the
relay is immediate. The relay is still tried first.

## Not covered

- `bus wait` and `bus explain` read activity from the local registry only.
- A message for an agent that left before it arrived is logged and dropped,
  not bounced back to its sender.

## Release order

The server (`www/api/v1/sync/secrets.js`, `_sync-indexes.js`) deploys before
the binaries. A binary ahead of it gets 400s on bus pushes and leaves them
dirty, which is harmless; they go out once the server is live.

A sender whose push response is lost retries, is refused, bumps past the
server's version and re-uploads over the receiver's tombstone. The claim
still stops a second delivery; the record just stays until the TTL.
