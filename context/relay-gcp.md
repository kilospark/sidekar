# Relay: build and deploy (GCP)

The WebSocket relay (`sidekar-relay`) lives in **`relay/`**. It bridges tunnel
connections from local Sidekar and browser viewers; it is **not** deployed on
Vercel.

The website still uses **`RELAY_URL`**, but production points at the custom
domain **`https://relay.sidekar.dev`**.

Sidekar now uses **owner-aware viewer routing** for web terminals. That means
each relay instance needs its own stable public origin so the browser can
connect directly to the relay that owns a given live session.

## Current production shape

- GCP project: `sidekar-prod-20260330`
- Zone: `us-central1-a`
- VMs:
  - `relay-vm`
    - machine type: `t2a-standard-1` (`ARM64`)
    - static IPv4: `34.10.236.43`
    - public origin: `https://relay.sidekar.dev`
    - startup env:
      - `RELAY_INSTANCE_ID=relay-vm`
      - `RELAY_PUBLIC_ORIGIN=https://relay.sidekar.dev`
  - `relay-vm-2`
    - machine type: `t2a-standard-1` (`ARM64`)
    - static IPv4: `34.171.111.17`
    - public origin: `https://relay2.sidekar.dev`
    - startup env:
      - `RELAY_INSTANCE_ID=relay-vm-2`
      - `RELAY_PUBLIC_ORIGIN=https://relay2.sidekar.dev`
- Artifact Registry repo:
  `us-central1-docker.pkg.dev/sidekar-prod-20260330/sidekar`
- Relay image tag used by the VM startup script:
  `us-central1-docker.pkg.dev/sidekar-prod-20260330/sidekar/relay:arm64`
- Secrets in Secret Manager:
  - `relay-mongodb-uri`
  - `relay-jwt-secret`

Each VM uses startup-script metadata to:

- install Docker and runtime tools
- fetch the two secrets from Secret Manager
- run the relay on `127.0.0.1:8080`
- run `caddy:2` on `80/443` for TLS and reverse proxy

Inspect the live startup script if needed:

```bash
gcloud compute instances describe relay-vm \
  --project=sidekar-prod-20260330 \
  --zone=us-central1-a \
  --format='get(metadata.items.startup-script)'

gcloud compute instances describe relay-vm-2 \
  --project=sidekar-prod-20260330 \
  --zone=us-central1-a \
  --format='get(metadata.items.startup-script)'
```

## Build locally

```bash
cd relay
cargo build --release
# output: target/release/sidekar-relay
```

Run locally with production-style env:

```bash
MONGODB_URI="..." JWT_SECRET="..." cargo run
```

The HTTP server listens on **`PORT`** if set, otherwise defaults to `8080`.

## Build and push the production image

Do **not** submit `relay/target/` as build context. Stage a clean temp context
first:

```bash
rm -rf /tmp/sidekar-relay-build
mkdir -p /tmp/sidekar-relay-build
rsync -a --exclude target relay/ /tmp/sidekar-relay-build/
```

Build and push the ARM image:

```bash
docker buildx build \
  --platform linux/arm64 \
  -t us-central1-docker.pkg.dev/sidekar-prod-20260330/sidekar/relay:arm64 \
  --push \
  /tmp/sidekar-relay-build
```

If Docker is not running locally, start Colima first:

```bash
colima start
gcloud auth configure-docker us-central1-docker.pkg.dev
```

## Secrets

Relay runtime secrets live in GCP Secret Manager:

```bash
printf '%s' 'mongodb+srv://...' | gcloud secrets versions add relay-mongodb-uri \
  --project=sidekar-prod-20260330 \
  --data-file=-

printf '%s' 'jwt-secret' | gcloud secrets versions add relay-jwt-secret \
  --project=sidekar-prod-20260330 \
  --data-file=-
```

Important: **`JWT_SECRET` is shared between the relay and the Vercel website**.
If you rotate it for the relay, also rotate `JWT_SECRET` in the Vercel project
and redeploy `www/`.

## Roll out a new relay build

After pushing a new `:arm64` image, reboot both VMs so each startup script
pulls the new image and restarts both containers:

```bash
gcloud compute instances reset relay-vm \
  --project=sidekar-prod-20260330 \
  --zone=us-central1-a

gcloud compute instances reset relay-vm-2 \
  --project=sidekar-prod-20260330 \
  --zone=us-central1-a
```

This is an active-active deployment for relay ownership, not a generic
round-robin pool. Both relays run the same image and share MongoDB, but each
live session has one owning relay instance recorded in Mongo. The browser
resolves that owner and connects to the owning relay's public origin.

## Verify

Check the relays directly on both VMs:

```bash
gcloud compute ssh relay-vm \
  --project=sidekar-prod-20260330 \
  --zone=us-central1-a \
  --command='sudo docker ps --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}"'

gcloud compute ssh relay-vm \
  --project=sidekar-prod-20260330 \
  --zone=us-central1-a \
  --command='curl -fsS http://127.0.0.1:8080/health'

gcloud compute ssh relay-vm-2 \
  --project=sidekar-prod-20260330 \
  --zone=us-central1-a \
  --command='sudo docker ps --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}"'

gcloud compute ssh relay-vm-2 \
  --project=sidekar-prod-20260330 \
  --zone=us-central1-a \
  --command='curl -fsS http://127.0.0.1:8080/health'
```

Check the public endpoints on the GCP IPs:

```bash
curl -fsS --resolve relay.sidekar.dev:443:34.10.236.43 \
  https://relay.sidekar.dev/health

curl -fsS --resolve relay2.sidekar.dev:443:34.171.111.17 \
  https://relay2.sidekar.dev/health
```

Then verify the public domains normally:

```bash
dig +short A relay.sidekar.dev
curl -fsS https://relay.sidekar.dev/health

dig +short A relay2.sidekar.dev
curl -fsS https://relay2.sidekar.dev/health
```

## DNS

`relay.sidekar.dev` and `relay2.sidekar.dev` are managed in **Vercel DNS**, not
GCP DNS.

Current production records:

- `relay.sidekar.dev A 34.10.236.43`
- `relay2.sidekar.dev A 34.171.111.17`

There should be **no** `AAAA` record for either relay hostname.

Useful commands:

```bash
vercel dns ls sidekar.dev --scope kxbnb
vercel dns add sidekar.dev relay A 34.10.236.43 --scope kxbnb
vercel dns add sidekar.dev relay2 A 34.171.111.17 --scope kxbnb
```

If TLS issuance ever fails right after a cutover, check whether public DNS still
shows an old address. Caddy's ACME flow will fail until resolvers see only the
new IP.

## Multi-instance note

Do not hide both relays behind a single generic load-balancer origin unless the
load balancer itself becomes owner-aware. The current design stores an
`owner_origin` for each live session in Mongo and sends the browser directly to
that relay instance. A single shared origin would lose that routing signal.

## Historical note

Fly.io was retired for the relay on **March 31, 2026**. There is no longer a
production Fly deployment for `sidekar-relay`.

## Related

- **`context/www-vercel.md`** — Vercel deployment for `www/`
- **`docs/superpowers/plans/2026-03-23-web-terminal-tunnel.md`** — tunnel /
  terminal architecture and API notes
