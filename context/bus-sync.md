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
out without anyone tombstoning its agents.

## Messages (`bus` records)

`bus send` resolves a name in this order: this channel, this machine, a
relay session, then another machine's published agent. The last queues a
`bus` record (id: the message id) addressed to that agent's device in
`bus_outbox`, and pushes it at once. The push failing leaves it queued;
the daemon's next round retries.

The recipient's daemon delivers it as the relay delivers a tunnelled message:
a request is set pending, an answer is recorded against its request, and the
text goes into the local queue (only if the recipient is registered there;
otherwise it would reach the next agent to take the name). It then
tombstones the record. A pulled message already in `sync_state` is never
delivered twice.

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
