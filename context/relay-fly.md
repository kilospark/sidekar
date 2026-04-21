# Relay: build and deploy (Fly.io)

The WebSocket relay (`sidekar-relay`) lives in **`relay/`**. It bridges
tunnel connections from local Sidekar to browser viewers and Telegram
chats; it is **not** deployed on Vercel.

Production runs as a single Fly.io machine under the app
**`sidekar-relay`** in region `sjc`. The website points at the custom
domain **`https://relay.sidekar.dev`**.

## Current production shape

- Fly app: **`sidekar-relay`**
- Region: **`sjc`** (primary)
- Machines: **1** (`shared-cpu-1x`, 256 MB) — single-instance deploy,
  under $5/mo.
- Public domain: `https://relay.sidekar.dev` (CNAME → Fly app host)
- MongoDB: existing Atlas cluster (not hosted on Fly).
- Secrets (set via `flyctl secrets set`, never committed):
  - `MONGODB_URI`
  - `JWT_SECRET`
  - `TELEGRAM_BOT_TOKEN`          (optional — enables Telegram)
  - `TELEGRAM_WEBHOOK_SECRET`     (required iff Telegram enabled)
  - `TELEGRAM_BOT_USERNAME`       (optional; defaults `sidekar_bot`)

Non-secret env is pinned in `fly.toml` → `[env]`:

```
PORT                 = 8080
RELAY_INSTANCE_ID    = fly-sjc
RELAY_PUBLIC_ORIGIN  = https://relay.sidekar.dev
```

Owner-aware viewer routing degenerates to "Local" for every viewer on
a single-machine deploy. The cross-instance path (`/telegram/deliver`,
`ViewerRoute::Remote`) is still in the binary but never fires.

## Prereqs

- `flyctl` installed (`brew install flyctl`).
- Authenticated: `flyctl auth login` (opens browser).

## Initial setup (first deploy only)

```bash
cd relay
flyctl apps create sidekar-relay          # only if it doesn't exist
flyctl volumes ls                         # we use none currently
flyctl secrets set \
  MONGODB_URI='mongodb+srv://...' \
  JWT_SECRET='…' \
  TELEGRAM_BOT_TOKEN='…' \
  TELEGRAM_WEBHOOK_SECRET='…' \
  TELEGRAM_BOT_USERNAME='sidekar_bot'
flyctl deploy
```

Custom-domain cert:

```bash
flyctl certs add relay.sidekar.dev
flyctl certs show relay.sidekar.dev
```

DNS: point `relay.sidekar.dev` at the Fly app using one of Fly's
recommended record shapes (CNAME to `sidekar-relay.fly.dev` plus AAAA
for IPv6-only hosts, or direct A/AAAA). `flyctl certs show` prints the
exact records.

## Redeploy

```bash
cd relay
flyctl deploy
```

`flyctl deploy` builds the image remotely using `Dockerfile`, pushes
to Fly's registry, and rolls the machine. Expect ~2 minutes for a
cold build, under a minute on incremental.

## Secrets rotation

```bash
flyctl secrets set JWT_SECRET='new-value'
flyctl secrets unset TELEGRAM_BOT_TOKEN   # disables Telegram
flyctl secrets list                        # names + digests, no values
```

Secret changes trigger a rolling restart automatically.

## Logs + status

```bash
flyctl status
flyctl logs
flyctl ssh console                 # interactive shell on the machine
```

## Scaling (if ever needed)

```bash
flyctl scale count 2 --region sjc  # adds a second machine, same region
flyctl scale vm shared-cpu-2x      # bump CPU tier
flyctl scale memory 512            # MB
```

Note: scaling to >1 machine means `RELAY_INSTANCE_ID` must differ per
machine. Fly injects a unique machine id via `FLY_MACHINE_ID`; update
`fly.toml`'s `[env]` to `RELAY_INSTANCE_ID = "$FLY_MACHINE_ID"` (or
just unset and let `main.rs` fall back to `HOSTNAME`).

## Local dev run

```bash
cd relay
MONGODB_URI='...' JWT_SECRET='dev' cargo run
```

## GCP teardown history

Relay previously ran on two GCP VMs (`relay-vm` / `relay-vm-2`) in
`us-central1-a`. After the Fly cutover the VMs were stopped. The
Artifact Registry image
`us-central1-docker.pkg.dev/sidekar-prod-20260330/sidekar/relay:arm64`
is kept for rollback; delete after a clean operating window. The
static IPs (`34.10.236.43`, `34.171.111.17`) can be released after
DNS TTL fully rolls over.

Secrets `relay-mongodb-uri` and `relay-jwt-secret` in GCP Secret
Manager are now orphaned — delete when confident in Fly.

## Tunnel URL

Sidekar CLI connects to `wss://relay.sidekar.dev/tunnel` by default
(see `src/tunnel.rs` `DEFAULT_RELAY_URL`). Override via the
`SIDEKAR_RELAY_URL` env var for testing against a staging app.
