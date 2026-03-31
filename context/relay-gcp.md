# Relay: build and deploy (GCP)

The WebSocket relay (`sidekar-relay`) lives in **`relay/`**. It bridges tunnel
connections from local Sidekar and browser viewers; it is **not** deployed on
Vercel.

The website still uses **`RELAY_URL`**, but production points at the custom
domain **`https://relay.sidekar.dev`**.

## Current production shape

- GCP project: `sidekar-prod-20260330`
- Zone: `us-central1-a`
- VM: `relay-vm`
- Machine type: `t2a-standard-1` (`ARM64`)
- Static IPv4: `34.10.236.43`
- Artifact Registry repo:
  `us-central1-docker.pkg.dev/sidekar-prod-20260330/sidekar`
- Relay image tag used by the VM startup script:
  `us-central1-docker.pkg.dev/sidekar-prod-20260330/sidekar/relay:arm64`
- Secrets in Secret Manager:
  - `relay-mongodb-uri`
  - `relay-jwt-secret`

The VM uses startup-script metadata to:

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

After pushing a new `:arm64` image, reboot the VM so the startup script pulls
the new image and restarts both containers:

```bash
gcloud compute instances reset relay-vm \
  --project=sidekar-prod-20260330 \
  --zone=us-central1-a
```

## Verify

Check the relay directly on the VM:

```bash
gcloud compute ssh relay-vm \
  --project=sidekar-prod-20260330 \
  --zone=us-central1-a \
  --command='sudo docker ps --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}"'

gcloud compute ssh relay-vm \
  --project=sidekar-prod-20260330 \
  --zone=us-central1-a \
  --command='curl -fsS http://127.0.0.1:8080/health'
```

Check the public endpoint on the GCP IP:

```bash
curl -fsS --resolve relay.sidekar.dev:443:34.10.236.43 \
  https://relay.sidekar.dev/health
```

Then verify the public domain normally:

```bash
dig +short A relay.sidekar.dev
curl -fsS https://relay.sidekar.dev/health
```

## DNS

`relay.sidekar.dev` is managed in **Vercel DNS**, not GCP DNS.

Current production record:

- `relay.sidekar.dev A 34.10.236.43`

There should be **no** `AAAA` record for `relay.sidekar.dev`.

Useful commands:

```bash
vercel dns ls sidekar.dev --scope kxbnb
vercel dns add sidekar.dev relay A 34.10.236.43 --scope kxbnb
```

If TLS issuance ever fails right after a cutover, check whether public DNS still
shows an old address. Caddy's ACME flow will fail until resolvers see only the
new IP.

## Historical note

Fly.io was retired for the relay on **March 31, 2026**. There is no longer a
production Fly deployment for `sidekar-relay`.

## Related

- **`context/www-vercel.md`** — Vercel deployment for `www/`
- **`docs/superpowers/plans/2026-03-23-web-terminal-tunnel.md`** — tunnel /
  terminal architecture and API notes
