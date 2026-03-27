# Relay: build and deploy (Fly.io)

The WebSocket relay (`sidekar-relay`) lives in **`relay/`**. It bridges tunnel connections from local Sidekar and browser viewers; it is **not** deployed on Vercel. The website uses **`RELAY_URL`** to proxy session list / health as needed.

## Build locally

```bash
cd relay
cargo build --release
# output: target/release/sidekar-relay
```

Run locally (set env to match production secrets):

```bash
MONGODB_URI="..." JWT_SECRET="..." cargo run
```

The HTTP server listens on **`PORT`** if set, otherwise the default in `main.rs` (Fly sets **`8080`** via `fly.toml` `internal_port`).

## Docker

The image is built from **`relay/Dockerfile`** (build context must be **`relay/`** — it only copies `Cargo.toml`, `Cargo.lock`, and `src/`).

```bash
cd relay
docker build -t sidekar-relay .
```

## Deploy to Fly.io

App name and region are in **`relay/fly.toml`** (`app = "sidekar-relay"`, `primary_region = "sjc"`).

From **`relay/`**:

```bash
fly deploy
```

Set secrets once (or when rotating):

```bash
fly secrets set MONGODB_URI="..." JWT_SECRET="..."
```

Check status:

```bash
fly status
curl -fsS https://sidekar-relay.fly.dev/health
```

Custom domain **`relay.sidekar.dev`** is configured in Fly / DNS separately from this repo.

## Related

- **`context/www-vercel.md`** — Vercel deployment for `www/`; set **`RELAY_URL`** to the Fly relay HTTPS base URL.
- **`docs/superpowers/plans/2026-03-23-web-terminal-tunnel.md`** — full tunnel / terminal architecture and API notes.
