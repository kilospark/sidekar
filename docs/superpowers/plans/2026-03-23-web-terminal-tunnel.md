# Web Terminal Tunnel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users access their `sidekar <agent>` PTY sessions from a browser on any device, authenticated via GitHub OAuth through sidekar.dev.

**Architecture:** Three components: (1) a Rust relay binary on Fly.io that bridges WSS tunnel connections from local sidekar instances with browser WebSocket connections, (2) new Vercel serverless API routes and pages on sidekar.dev for GitHub OAuth, device authorization, session dashboard, and xterm.js terminal UI, (3) extensions to the sidekar CLI to authenticate, establish tunnel connections, and fan-out PTY data to the relay.

**Tech Stack:** Rust (relay binary + CLI changes), tokio-tungstenite (WebSocket), MongoDB Atlas (users/devices), Vercel serverless functions (auth API), xterm.js (browser terminal), GitHub OAuth (authentication), Fly.io (relay hosting)

---

## System Overview

```
Browser (sidekar.dev)                    Fly.io                         Local machine
┌─────────────────────┐          ┌─────────────────────┐       ┌──────────────────────┐
│ /sessions            │          │  sidekar-relay       │       │  sidekar codex       │
│   dashboard listing  │          │                      │       │    ├─ PTY (codex)     │
│                      │          │  session registry    │       │    ├─ event loop      │
│ /terminal/:id        │◄─WSS───►│  (in-memory map)     │◄─WSS──┤    └─ tunnel client   │
│   xterm.js           │          │                      │       │                       │
└─────────────────────┘          │  auth validation     │       │  ~/.config/sidekar/   │
                                 └─────────────────────┘       │    auth.json           │
┌─────────────────────┐                                         └──────────────────────┘
│ sidekar.dev/api      │
│  /api/auth/github    │  GitHub OAuth endpoints
│  /api/auth/device    │  Device authorization flow
│  /api/sessions       │  Proxy to relay for session list
└─────────────────────┘
```

## Data Model (MongoDB — `sidekar` database)

**`users` collection:**
```json
{
  "_id": ObjectId,
  "github_id": 12345,
  "login": "karthik",
  "name": "Karthik",
  "email": "...",
  "avatar_url": "https://avatars.githubusercontent.com/...",
  "created_at": ISODate,
  "last_login_at": ISODate
}
```

**`devices` collection:**
```json
{
  "_id": ObjectId,
  "user_id": ObjectId,
  "token_hash": "sha256:...",
  "hostname": "MacBook-Pro.local",
  "os": "darwin",
  "arch": "arm64",
  "sidekar_version": "0.4.22",
  "last_seen_at": ISODate,
  "created_at": ISODate
}
```

**`device_codes` collection (ephemeral, TTL-indexed):**
```json
{
  "_id": ObjectId,
  "device_code": "random-32-char",
  "user_code": "ABCD-1234",
  "user_id": null,
  "expires_at": ISODate,
  "created_at": ISODate
}
```
TTL index on `expires_at` — MongoDB auto-deletes expired codes.

**Sessions are NOT in MongoDB.** They exist only in-memory on the relay while the tunnel WSS is connected.

## Authentication Flows

### Browser login (GitHub OAuth)
```
1. User clicks "Sign in with GitHub" on sidekar.dev
2. Redirect to github.com/login/oauth/authorize?client_id=...&scope=read:user,user:email
3. GitHub redirects back to /api/auth/github/callback?code=...
4. Server exchanges code for access token, fetches user profile
5. Upsert user in MongoDB, set session cookie (signed JWT)
6. Redirect to /sessions dashboard
```

### Device authorization (sidekar CLI)
```
1. sidekar cli: POST /api/auth/device → { device_code, user_code, verification_uri }
2. sidekar cli: opens browser to verification_uri, prints user_code
3. User sees: "Enter code: ABCD-1234" on sidekar.dev, approves
4. sidekar cli: polls POST /api/auth/device/token { device_code }
5. Server returns { token } once user approves (or "pending" / "expired")
6. sidekar cli: stores token in ~/.config/sidekar/auth.json
```

### Relay authentication
```
Tunnel connect:   WSS + Authorization: Bearer <device_token>
Browser connect:  WSS + cookie (session JWT) or query param token
Relay validates both against MongoDB on connect.
```

## Relay Protocol

### Tunnel connection (sidekar CLI → relay)
```
Connect: wss://relay.sidekar.dev/tunnel
Headers: Authorization: Bearer <device_token>

→ {"type":"register","session_name":"codex-sidekar-1","agent_type":"codex","cwd":"/Users/karthik/src/foo","hostname":"MacBook-Pro.local"}
← {"type":"registered","session_id":"uuid"}

Bidirectional binary frames: raw PTY bytes
Control frames (JSON text):
→ {"type":"resize","cols":80,"rows":24}
← {"type":"viewer_connected","count":1}
← {"type":"viewer_disconnected","count":0}
```

### Browser connection (xterm.js → relay)
```
Connect: wss://relay.sidekar.dev/session/<session_id>
Headers: Cookie: sidekar_session=<jwt>

Bidirectional binary frames: raw PTY bytes
Control frames (JSON text):
→ {"type":"resize","cols":80,"rows":24}
```

### Session list (browser → relay, HTTP)
```
GET https://relay.sidekar.dev/sessions
Headers: Cookie: sidekar_session=<jwt>

← {"sessions":[{"id":"uuid","name":"codex-sidekar-1","agent_type":"codex","cwd":"...","hostname":"...","connected_at":"...","viewers":0}]}
```

## File Structure

### New: relay binary (`relay/`)
```
relay/
  Cargo.toml              — separate binary crate (not workspace, standalone)
  src/
    main.rs               — entry point, CLI args, starts server
    server.rs             — HTTP + WS server (axum or warp)
    auth.rs               — token/JWT validation against MongoDB
    registry.rs           — in-memory session registry
    bridge.rs             — bridges tunnel WSS ↔ browser WSS
    types.rs              — shared message types
  Dockerfile              — for Fly.io deployment
  fly.toml                — Fly.io config
```

### Modified: sidekar CLI (`src/`)
```
src/
  tunnel.rs               — NEW: tunnel client (WSS connect, reconnect, auth)
  auth.rs                 — NEW: device auth flow (POST /api/auth/device, poll, store token)
  pty.rs                  — MODIFIED: fan-out master fd to tunnel + stdout
  api_client.rs           — MODIFIED: add auth endpoints
```

### New: sidekar.dev pages and API (`www/`)
```
www/
  api/
    auth/
      github.js           — GET: redirect to GitHub OAuth
      github/
        callback.js       — GET: handle OAuth callback, set cookie
      device.js           — POST: create device code
      device/
        token.js          — POST: poll for device token
        approve.js        — POST: approve device code (from browser)
      me.js               — GET: current user from cookie
      logout.js           — POST: clear cookie
    sessions.js            — GET: proxy to relay /sessions endpoint
  public/
    sessions.html          — dashboard: list active sessions
    terminal.html          — xterm.js terminal page
    approve.html           — device code approval page
    js/
      terminal.js          — xterm.js + WebSocket glue
      sessions.js          — dashboard JS
      approve.js           — approval page JS
  init-db.js               — MODIFIED: add users, devices, device_codes indexes
```

---

## Task 1: MongoDB Schema and Indexes

**Files:**
- Modify: `www/init-db.js`
- Modify: `www/api/_db.js`

- [ ] **Step 1: Add users, devices, and device_codes collection indexes to init-db.js**

```javascript
// Add after existing feedback indexes:

// Users indexes
await db.collection("users").createIndex({ github_id: 1 }, { unique: true });
await db.collection("users").createIndex({ login: 1 });
console.log("  users: github_id (unique), login");

// Devices indexes
await db.collection("devices").createIndex({ user_id: 1 });
await db.collection("devices").createIndex({ token_hash: 1 }, { unique: true });
console.log("  devices: user_id, token_hash (unique)");

// Device codes indexes (TTL: auto-delete after expires_at)
await db.collection("device_codes").createIndex({ expires_at: 1 }, { expireAfterSeconds: 0 });
await db.collection("device_codes").createIndex({ device_code: 1 }, { unique: true });
await db.collection("device_codes").createIndex({ user_code: 1 });
console.log("  device_codes: expires_at (TTL), device_code (unique), user_code");
```

- [ ] **Step 2: Run init-db.js against Atlas to create indexes**

```bash
cd www && MONGODB_URI="$(grep MONGODB_URI .env | cut -d'"' -f2)" node init-db.js
```

Expected: indexes created without errors.

- [ ] **Step 3: Commit**

```bash
git add www/init-db.js
git commit -m "feat: add MongoDB indexes for users, devices, and device_codes collections"
```

---

## Task 2: GitHub OAuth API Routes

**Files:**
- Create: `www/api/auth/github.js`
- Create: `www/api/auth/github/callback.js`
- Create: `www/api/auth/me.js`
- Create: `www/api/auth/logout.js`
- Modify: `www/vercel.json`
- Modify: `www/package.json`

Requires Vercel env vars: `GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET`, `JWT_SECRET` (random 64-char hex).

- [ ] **Step 1: Add jose dependency to www/package.json**

```json
{
  "dependencies": {
    "mongodb": "^6.13.1",
    "jose": "^5.2.0"
  }
}
```

- [ ] **Step 2: Create JWT helper (www/api/_auth.js)**

```javascript
import { SignJWT, jwtVerify } from "jose";

const JWT_SECRET = new TextEncoder().encode(process.env.JWT_SECRET || "dev-secret-change-me");
const COOKIE_NAME = "sidekar_session";

export async function signToken(payload) {
  return new SignJWT(payload)
    .setProtectedHeader({ alg: "HS256" })
    .setExpirationTime("30d")
    .sign(JWT_SECRET);
}

export async function verifyToken(token) {
  try {
    const { payload } = await jwtVerify(token, JWT_SECRET);
    return payload;
  } catch {
    return null;
  }
}

export function parseCookie(req) {
  const header = req.headers.cookie || "";
  const match = header.match(new RegExp(`${COOKIE_NAME}=([^;]+)`));
  return match ? match[1] : null;
}

export async function getUser(req) {
  const token = parseCookie(req);
  if (!token) return null;
  return verifyToken(token);
}

export function setSessionCookie(res, token) {
  res.setHeader("Set-Cookie", `${COOKIE_NAME}=${token}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=${30 * 24 * 60 * 60}`);
}

export function clearSessionCookie(res) {
  res.setHeader("Set-Cookie", `${COOKIE_NAME}=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0`);
}
```

- [ ] **Step 3: Create www/api/auth/github.js (redirect to GitHub)**

```javascript
export default async function handler(req, res) {
  if (req.method !== "GET") return res.status(405).end();

  const clientId = process.env.GITHUB_CLIENT_ID;
  if (!clientId) return res.status(500).json({ error: "GITHUB_CLIENT_ID not set" });

  const redirectUri = `https://sidekar.dev/api/auth/github/callback`;
  const scope = "read:user user:email";
  const url = `https://github.com/login/oauth/authorize?client_id=${clientId}&redirect_uri=${encodeURIComponent(redirectUri)}&scope=${encodeURIComponent(scope)}`;

  res.redirect(302, url);
}
```

- [ ] **Step 4: Create www/api/auth/github/callback.js (exchange code, upsert user, set cookie)**

```javascript
import { getDb } from "../../_db.js";
import { signToken, setSessionCookie } from "../../_auth.js";

export default async function handler(req, res) {
  if (req.method !== "GET") return res.status(405).end();

  const { code } = req.query;
  if (!code) return res.status(400).json({ error: "missing code" });

  // Exchange code for access token
  const tokenRes = await fetch("https://github.com/login/oauth/access_token", {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify({
      client_id: process.env.GITHUB_CLIENT_ID,
      client_secret: process.env.GITHUB_CLIENT_SECRET,
      code,
    }),
  });
  const tokenData = await tokenRes.json();
  if (tokenData.error) return res.status(400).json({ error: tokenData.error_description });

  // Fetch user profile
  const userRes = await fetch("https://api.github.com/user", {
    headers: { Authorization: `Bearer ${tokenData.access_token}`, Accept: "application/json" },
  });
  const ghUser = await userRes.json();

  // Fetch primary email if not public
  let email = ghUser.email;
  if (!email) {
    const emailsRes = await fetch("https://api.github.com/user/emails", {
      headers: { Authorization: `Bearer ${tokenData.access_token}`, Accept: "application/json" },
    });
    const emails = await emailsRes.json();
    const primary = emails.find((e) => e.primary && e.verified);
    email = primary ? primary.email : null;
  }

  // Upsert user
  const db = await getDb();
  const result = await db.collection("users").findOneAndUpdate(
    { github_id: ghUser.id },
    {
      $set: {
        login: ghUser.login,
        name: ghUser.name || ghUser.login,
        email,
        avatar_url: ghUser.avatar_url,
        last_login_at: new Date(),
      },
      $setOnInsert: { created_at: new Date() },
    },
    { upsert: true, returnDocument: "after" }
  );

  const user = result;
  const jwt = await signToken({
    sub: user._id.toString(),
    login: user.login,
    name: user.name,
  });

  setSessionCookie(res, jwt);
  res.redirect(302, "/sessions");
}
```

- [ ] **Step 5: Create www/api/auth/me.js and www/api/auth/logout.js**

`www/api/auth/me.js`:
```javascript
import { getUser } from "../_auth.js";

export default async function handler(req, res) {
  if (req.method !== "GET") return res.status(405).end();
  const user = await getUser(req);
  if (!user) return res.status(401).json({ error: "not authenticated" });
  res.json({ user });
}
```

`www/api/auth/logout.js`:
```javascript
import { clearSessionCookie } from "../_auth.js";

export default async function handler(req, res) {
  if (req.method !== "POST") return res.status(405).end();
  clearSessionCookie(res);
  res.json({ ok: true });
}
```

- [ ] **Step 6: Test locally**

```bash
cd www && npx vercel dev
# Visit http://localhost:3000/api/auth/github — should redirect to GitHub
# After callback, /api/auth/me should return user info
```

- [ ] **Step 7: Commit**

```bash
git add www/api/auth www/api/_auth.js www/package.json
git commit -m "feat: GitHub OAuth login for sidekar.dev"
```

---

## Task 3: Device Authorization API Routes

**Files:**
- Create: `www/api/auth/device.js`
- Create: `www/api/auth/device/token.js`
- Create: `www/api/auth/device/approve.js`
- Create: `www/public/approve.html`
- Create: `www/public/js/approve.js`

- [ ] **Step 1: Create www/api/auth/device.js (CLI calls this to start device flow)**

```javascript
import { randomBytes } from "crypto";
import { getDb } from "../_db.js";

export default async function handler(req, res) {
  if (req.method !== "POST") return res.status(405).end();

  const deviceCode = randomBytes(16).toString("hex");
  // User-facing code: 4 chars, dash, 4 chars (uppercase alphanumeric, no ambiguous chars)
  const chars = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // no 0/O/1/I
  let userCode = "";
  for (let i = 0; i < 8; i++) {
    if (i === 4) userCode += "-";
    userCode += chars[randomBytes(1)[0] % chars.length];
  }

  const db = await getDb();
  await db.collection("device_codes").insertOne({
    device_code: deviceCode,
    user_code: userCode,
    user_id: null,
    expires_at: new Date(Date.now() + 15 * 60 * 1000), // 15 min
    created_at: new Date(),
  });

  res.json({
    device_code: deviceCode,
    user_code: userCode,
    verification_uri: "https://sidekar.dev/approve",
    expires_in: 900,
    interval: 5,
  });
}
```

- [ ] **Step 2: Create www/api/auth/device/approve.js (browser user approves the code)**

```javascript
import { getDb } from "../../_db.js";
import { getUser } from "../../_auth.js";
import { randomBytes, createHash } from "crypto";

export default async function handler(req, res) {
  if (req.method !== "POST") return res.status(405).end();

  const user = await getUser(req);
  if (!user) return res.status(401).json({ error: "not authenticated" });

  const { user_code, hostname, os, arch, sidekar_version } = req.body;
  if (!user_code) return res.status(400).json({ error: "user_code required" });

  const db = await getDb();
  const doc = await db.collection("device_codes").findOne({
    user_code: user_code.toUpperCase().trim(),
    user_id: null,
  });

  if (!doc) return res.status(404).json({ error: "invalid or expired code" });

  // Generate device token
  const token = randomBytes(32).toString("hex");
  const tokenHash = createHash("sha256").update(token).digest("hex");

  // Create device record
  const { ObjectId } = await import("mongodb");
  await db.collection("devices").insertOne({
    user_id: new ObjectId(user.sub),
    token_hash: tokenHash,
    hostname: hostname || "unknown",
    os: os || "unknown",
    arch: arch || "unknown",
    sidekar_version: sidekar_version || "unknown",
    last_seen_at: new Date(),
    created_at: new Date(),
  });

  // Mark device code as approved (store token for polling endpoint)
  await db.collection("device_codes").updateOne(
    { _id: doc._id },
    { $set: { user_id: user.sub, token } }
  );

  res.json({ ok: true });
}
```

- [ ] **Step 3: Create www/api/auth/device/token.js (CLI polls this)**

```javascript
import { getDb } from "../../_db.js";

export default async function handler(req, res) {
  if (req.method !== "POST") return res.status(405).end();

  const { device_code } = req.body;
  if (!device_code) return res.status(400).json({ error: "device_code required" });

  const db = await getDb();
  const doc = await db.collection("device_codes").findOne({ device_code });

  if (!doc) return res.status(404).json({ error: "expired" });

  if (!doc.user_id) {
    // Not yet approved
    return res.json({ status: "pending" });
  }

  // Approved — return token and delete the device code
  const token = doc.token;
  await db.collection("device_codes").deleteOne({ _id: doc._id });

  res.json({ status: "approved", token });
}
```

- [ ] **Step 4: Create www/public/approve.html (user-facing approval page)**

Minimal page: text input for user code, submit button. Must be logged in (check /api/auth/me, redirect to /api/auth/github if not). On submit, POST to /api/auth/device/approve.

- [ ] **Step 5: Test the device flow manually**

```bash
# Terminal 1: start dev server
cd www && npx vercel dev

# Terminal 2: simulate CLI
curl -X POST http://localhost:3000/api/auth/device
# Returns device_code, user_code

# Browser: login via /api/auth/github, then go to /approve, enter user_code

# Terminal 2: poll
curl -X POST http://localhost:3000/api/auth/device/token -H 'Content-Type: application/json' -d '{"device_code":"<from step 1>"}'
# Should return {"status":"approved","token":"..."}
```

- [ ] **Step 6: Commit**

```bash
git add www/api/auth/device.js www/api/auth/device/ www/public/approve.html www/public/js/approve.js
git commit -m "feat: device authorization flow for sidekar CLI"
```

---

## Task 4: sidekar CLI — Auth Module

**Files:**
- Create: `src/auth.rs`
- Modify: `src/main.rs` (add `sidekar login` subcommand)
- Modify: `src/lib.rs` (declare `pub mod auth`)

- [ ] **Step 1: Create src/auth.rs**

Responsibilities:
- `auth_token() -> Option<String>` — read token from `~/.config/sidekar/auth.json`
- `save_token(token: &str)` — write token to auth.json
- `device_auth_flow()` — POST /api/auth/device, open browser, poll /api/auth/device/token, save token
- Token file format: `{"token":"hex","created_at":"iso"}`

Key implementation details:
- Use existing `api_client::api_base()` for URL
- Use `reqwest` (already a dependency)
- Open browser: `open` on macOS, `xdg-open` on Linux
- Poll every 5 seconds, timeout after 15 minutes
- Print user_code prominently to terminal

- [ ] **Step 2: Add `sidekar login` subcommand to main.rs**

```rust
if command == "login" {
    return sidekar::auth::device_auth_flow().await;
}
```

- [ ] **Step 3: Add mod declaration to lib.rs**

```rust
pub mod auth;
```

- [ ] **Step 4: Test manually**

```bash
cargo build && ./target/debug/sidekar login
# Should open browser, show user code, wait for approval
```

- [ ] **Step 5: Commit**

```bash
git add src/auth.rs src/main.rs src/lib.rs
git commit -m "feat: sidekar login — device authorization flow"
```

---

## Task 5: sidekar CLI — Tunnel Client

**Files:**
- Create: `src/tunnel.rs`
- Modify: `src/lib.rs` (declare `pub mod tunnel`)

- [ ] **Step 1: Create src/tunnel.rs**

Responsibilities:
- `TunnelClient` struct: holds WSS connection, session info
- `connect(token, session_name, agent_type, cwd) -> Result<TunnelClient>` — WSS connect to relay with auth header, send register message, wait for registered response
- `send_pty_data(&self, data: &[u8])` — send binary frame (PTY output)
- `recv() -> TunnelMessage` — receive: either binary (browser input) or control (resize, viewer count)
- `send_resize(cols, rows)` — send resize control frame
- Reconnect logic: on disconnect, exponential backoff (1s, 2s, 4s, max 30s), re-register

Key implementation details:
- Use `tokio-tungstenite` (already a dependency)
- `relay_url()` — `wss://relay.sidekar.dev/tunnel` (overridable via `SIDEKAR_RELAY_URL` env var)
- Binary frames for PTY data (low overhead)
- Text frames for JSON control messages
- Heartbeat: send ping every 30s, expect pong within 10s

- [ ] **Step 2: Add mod declaration to lib.rs**

```rust
pub mod tunnel;
```

- [ ] **Step 3: Commit**

```bash
git add src/tunnel.rs src/lib.rs
git commit -m "feat: tunnel client for WSS relay connection"
```

---

## Task 6: PTY Fan-Out to Tunnel

**Files:**
- Modify: `src/pty.rs`

- [ ] **Step 1: Modify run_agent() to optionally establish tunnel**

After broker registration and socket listener setup, check for auth token. If present, spawn tunnel connection as a background tokio task.

```rust
// After existing setup in run_agent():
let tunnel = if let Some(token) = crate::auth::auth_token() {
    match crate::tunnel::connect(&token, &identity.name, agent, &cwd_str).await {
        Ok(t) => {
            eprintln!("sidekar pty: tunnel connected");
            Some(t)
        }
        Err(e) => {
            eprintln!("sidekar pty: tunnel failed (continuing without): {e}");
            None
        }
    }
} else {
    None
};
```

- [ ] **Step 2: Modify event_loop() to fan-out and fan-in tunnel data**

Add tunnel to the tokio::select! loop:

- **master fd → stdout AND tunnel**: when reading from master fd, write to both stdout and tunnel (if connected)
- **tunnel → master fd**: when receiving binary data from tunnel (browser input), write to master fd
- **tunnel resize**: when receiving resize from tunnel, apply to PTY via ioctl
- **stdin still works**: local terminal input continues to work alongside tunnel input

- [ ] **Step 3: Handle tunnel reconnection in background**

Spawn a task that monitors tunnel health and reconnects on drop. The event_loop should handle tunnel being Some or None gracefully — tunnel failure never kills the local PTY session.

- [ ] **Step 4: Test locally**

```bash
# Start relay locally (Task 7)
# Login: sidekar login
# Launch: sidekar claude
# Should see "tunnel connected" message
# Typing locally should work as before
```

- [ ] **Step 5: Commit**

```bash
git add src/pty.rs
git commit -m "feat: fan-out PTY data to tunnel for web terminal access"
```

---

## Task 7: Relay Binary — Project Setup

**Files:**
- Create: `relay/Cargo.toml`
- Create: `relay/src/main.rs`
- Create: `relay/src/types.rs`

- [ ] **Step 1: Create relay/Cargo.toml**

```toml
[package]
name = "sidekar-relay"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
axum = { version = "0.8", features = ["ws"] }
axum-extra = { version = "0.10", features = ["typed-header"] }
tokio-tungstenite = "0.28"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
mongodb = "3"
jsonwebtoken = "9"
sha2 = "0.10"
uuid = { version = "1", features = ["v4"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tower-http = { version = "0.6", features = ["cors"] }
```

- [ ] **Step 2: Create relay/src/types.rs**

Shared types: `TunnelMessage`, `SessionInfo`, `ControlMessage` enums and structs.

- [ ] **Step 3: Create relay/src/main.rs with basic health check**

```rust
#[tokio::main]
async fn main() {
    tracing_subscriber::init();
    let app = axum::Router::new()
        .route("/health", axum::routing::get(|| async { "ok" }));
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".into());
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await.unwrap();
    tracing::info!("relay listening on {port}");
    axum::serve(listener, app).await.unwrap();
}
```

- [ ] **Step 4: Verify it builds and runs**

```bash
cd relay && cargo build && cargo run
# curl http://localhost:8080/health → "ok"
```

- [ ] **Step 5: Commit**

```bash
git add relay/
git commit -m "feat: sidekar-relay project scaffold with health check"
```

---

## Task 8: Relay — Auth Validation

**Files:**
- Create: `relay/src/auth.rs`

- [ ] **Step 1: Create relay/src/auth.rs**

Responsibilities:
- `validate_device_token(token) -> Option<UserId>` — SHA-256 hash token, look up in `devices` collection, return user_id. Update `last_seen_at`.
- `validate_session_jwt(jwt) -> Option<UserId>` — verify JWT signature (same `JWT_SECRET` as Vercel API), extract `sub` claim.
- MongoDB connection: connect on startup, share via `Arc<mongodb::Database>`.

- [ ] **Step 2: Commit**

```bash
git add relay/src/auth.rs
git commit -m "feat: relay auth — validate device tokens and session JWTs"
```

---

## Task 9: Relay — Session Registry

**Files:**
- Create: `relay/src/registry.rs`

- [ ] **Step 1: Create relay/src/registry.rs**

```rust
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct Session {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub agent_type: String,
    pub cwd: String,
    pub hostname: String,
    pub connected_at: chrono::DateTime<chrono::Utc>,
    pub tunnel_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    pub viewers: Arc<RwLock<Vec<ViewerHandle>>>,
}

pub struct ViewerHandle {
    pub tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
}

pub struct Registry {
    sessions: RwLock<HashMap<String, Session>>,  // session_id → Session
    user_sessions: RwLock<HashMap<String, Vec<String>>>,  // user_id → [session_id]
}
```

Methods:
- `register(user_id, session_info, tunnel_tx) -> session_id`
- `unregister(session_id)`
- `get_sessions(user_id) -> Vec<SessionInfo>`
- `add_viewer(session_id, viewer_tx) -> Option<mpsc::UnboundedReceiver<Vec<u8>>>`
- `remove_viewer(session_id, viewer_id)`

- [ ] **Step 2: Commit**

```bash
git add relay/src/registry.rs
git commit -m "feat: relay session registry — in-memory session and viewer tracking"
```

---

## Task 10: Relay — WebSocket Bridge

**Files:**
- Create: `relay/src/bridge.rs`
- Modify: `relay/src/main.rs` (add routes)

- [ ] **Step 1: Create relay/src/bridge.rs**

Two WebSocket handlers:

`handle_tunnel(ws, auth, registry)`:
- Validate device token from Authorization header
- Read register message, create session in registry
- Loop: binary frames from tunnel → broadcast to all viewers; binary frames from viewers → forward to tunnel
- On disconnect: unregister session

`handle_viewer(ws, session_id, auth, registry)`:
- Validate session JWT from cookie/query
- Look up session, verify user_id matches
- Add viewer to session
- Loop: binary frames from tunnel → send to this viewer; binary frames from viewer → send to tunnel
- On disconnect: remove viewer

- [ ] **Step 2: Add routes to main.rs**

```rust
let app = Router::new()
    .route("/health", get(|| async { "ok" }))
    .route("/tunnel", get(handle_tunnel_upgrade))
    .route("/session/{id}", get(handle_viewer_upgrade))
    .route("/sessions", get(handle_list_sessions));
```

- [ ] **Step 3: Add terminal replay buffer**

Keep last 50KB of tunnel output per session in a ring buffer. On viewer connect, send the buffer first so the viewer sees the current terminal state.

- [ ] **Step 4: Test locally with websocat**

```bash
# Terminal 1: start relay
cd relay && MONGODB_URI="..." JWT_SECRET="..." cargo run

# Terminal 2: simulate tunnel
websocat ws://localhost:8080/tunnel -H "Authorization: Bearer <token>"
# Send: {"type":"register","session_name":"test","agent_type":"test","cwd":"/tmp","hostname":"test"}

# Terminal 3: check sessions
curl http://localhost:8080/sessions -H "Cookie: sidekar_session=<jwt>"
```

- [ ] **Step 5: Commit**

```bash
git add relay/src/bridge.rs relay/src/main.rs
git commit -m "feat: relay WebSocket bridge — tunnel and viewer handlers with replay buffer"
```

---

## Task 11: Relay — Fly.io Deployment

**Files:**
- Create: `relay/Dockerfile`
- Create: `relay/fly.toml`

- [ ] **Step 1: Create relay/Dockerfile**

```dockerfile
FROM rust:1.83-slim AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/sidekar-relay /usr/local/bin/
CMD ["sidekar-relay"]
```

- [ ] **Step 2: Create relay/fly.toml**

```toml
app = "sidekar-relay"
primary_region = "sjc"

[build]

[http_service]
  internal_port = 8080
  force_https = true
  auto_stop_machines = "off"    # relay must stay running
  auto_start_machines = true
  min_machines_running = 1

[[vm]]
  size = "shared-cpu-1x"
  memory = "256mb"
```

- [ ] **Step 3: Deploy to Fly.io**

```bash
cd relay
fly launch --no-deploy  # creates app
fly secrets set MONGODB_URI="..." JWT_SECRET="..."
fly deploy
fly status  # verify running
curl https://sidekar-relay.fly.dev/health  # → "ok"
```

- [ ] **Step 4: Commit**

```bash
git add relay/Dockerfile relay/fly.toml
git commit -m "feat: relay Fly.io deployment config"
```

---

## Task 12: sidekar.dev — Sessions Dashboard

**Files:**
- Create: `www/public/sessions.html`
- Create: `www/public/js/sessions.js`
- Create: `www/api/sessions.js`

- [ ] **Step 1: Create www/api/sessions.js (proxy to relay)**

```javascript
import { getUser } from "./_auth.js";

const RELAY_URL = process.env.RELAY_URL || "https://sidekar-relay.fly.dev";

export default async function handler(req, res) {
  if (req.method !== "GET") return res.status(405).end();

  const user = await getUser(req);
  if (!user) return res.status(401).json({ error: "not authenticated" });

  // Forward the session cookie to the relay
  const relayRes = await fetch(`${RELAY_URL}/sessions`, {
    headers: { Cookie: req.headers.cookie },
  });
  const data = await relayRes.json();
  res.json(data);
}
```

- [ ] **Step 2: Create www/public/sessions.html**

Dashboard page: fetch /api/sessions, render session list. Each session shows name, agent_type, hostname, cwd, connected time, viewer count. Click → navigates to /terminal/<session_id>. Styled consistent with existing sidekar.dev (dark theme, Inter font, zinc tokens).

- [ ] **Step 3: Create www/public/js/sessions.js**

Fetch sessions, render cards, auto-refresh every 10s. Handle empty state ("No active sessions. Run `sidekar <agent>` to get started."). Handle not-authenticated (redirect to /api/auth/github).

- [ ] **Step 4: Commit**

```bash
git add www/public/sessions.html www/public/js/sessions.js www/api/sessions.js
git commit -m "feat: sessions dashboard on sidekar.dev"
```

---

## Task 13: sidekar.dev — Terminal Page

**Files:**
- Create: `www/public/terminal.html`
- Create: `www/public/js/terminal.js`

- [ ] **Step 1: Create www/public/terminal.html**

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>sidekar — terminal</title>
  <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/@xterm/xterm@5/css/xterm.css">
  <style>
    body { margin: 0; background: #09090b; overflow: hidden; }
    #terminal { width: 100vw; height: 100vh; }
    #status { position: fixed; top: 8px; right: 12px; font-size: 12px; color: #71717a; font-family: monospace; z-index: 10; }
  </style>
</head>
<body>
  <div id="status">connecting...</div>
  <div id="terminal"></div>
  <script type="module" src="https://cdn.jsdelivr.net/npm/@xterm/xterm@5/lib/xterm.min.js"></script>
  <script type="module" src="https://cdn.jsdelivr.net/npm/@xterm/addon-fit@0/lib/addon-fit.min.js"></script>
  <script type="module" src="https://cdn.jsdelivr.net/npm/@xterm/addon-web-links@0/lib/addon-web-links.min.js"></script>
  <script type="module" src="/js/terminal.js"></script>
</body>
</html>
```

- [ ] **Step 2: Create www/public/js/terminal.js**

```javascript
// Extract session ID from URL: /terminal/<id>
const sessionId = window.location.pathname.split("/").pop();
const relayHost = "relay.sidekar.dev"; // or from env

const term = new Terminal({
  cursorBlink: true,
  fontSize: 14,
  fontFamily: "'SF Mono', Menlo, Consolas, monospace",
  theme: { background: "#09090b" },
});

const fitAddon = new FitAddon.FitAddon();
term.loadAddon(fitAddon);
term.loadAddon(new WebLinksAddon.WebLinksAddon());
term.open(document.getElementById("terminal"));
fitAddon.fit();

const status = document.getElementById("status");

function connect() {
  const ws = new WebSocket(`wss://${relayHost}/session/${sessionId}`);
  ws.binaryType = "arraybuffer";

  ws.onopen = () => {
    status.textContent = "connected";
    // Send initial size
    ws.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
  };

  ws.onmessage = (event) => {
    if (typeof event.data === "string") {
      // Control message (ignore for now)
      return;
    }
    term.write(new Uint8Array(event.data));
  };

  ws.onclose = () => {
    status.textContent = "disconnected — reconnecting...";
    setTimeout(connect, 2000);
  };

  ws.onerror = () => ws.close();

  // Send user input to relay
  term.onData((data) => {
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(new TextEncoder().encode(data));
    }
  });

  // Send resize events
  window.addEventListener("resize", () => {
    fitAddon.fit();
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
    }
  });
}

connect();
```

- [ ] **Step 3: Test end-to-end**

```bash
# 1. Start relay locally or use Fly.io deployment
# 2. sidekar login (if not already)
# 3. sidekar claude
# 4. Open browser to /sessions, see the session, click it
# 5. xterm.js should show the same terminal output as local
# 6. Type in browser — input reaches claude
# 7. Type locally — browser sees the output
```

- [ ] **Step 4: Commit**

```bash
git add www/public/terminal.html www/public/js/terminal.js
git commit -m "feat: xterm.js terminal page on sidekar.dev"
```

---

## Task 14: Vercel Routing and Deployment

**Files:**
- Modify: `www/vercel.json`

- [ ] **Step 1: Add rewrites for new pages**

```json
{
  "rewrites": [
    { "source": "/v1/:path*", "destination": "/api/v1/:path*" },
    { "source": "/download/v:version/:asset", "destination": "/binaries/v:version/:asset" },
    { "source": "/download/:version/:asset", "destination": "/binaries/v:version/:asset" },
    { "source": "/install", "destination": "/api/script?name=install" },
    { "source": "/install.sh", "destination": "/api/script?name=install" },
    { "source": "/uninstall", "destination": "/api/script?name=uninstall" },
    { "source": "/uninstall.sh", "destination": "/api/script?name=uninstall" },
    { "source": "/sessions", "destination": "/sessions.html" },
    { "source": "/terminal/:id", "destination": "/terminal.html" },
    { "source": "/approve", "destination": "/approve.html" }
  ]
}
```

- [ ] **Step 2: Set Vercel env vars**

```bash
vercel env add GITHUB_CLIENT_ID
vercel env add GITHUB_CLIENT_SECRET
vercel env add JWT_SECRET
vercel env add RELAY_URL  # https://sidekar-relay.fly.dev
```

- [ ] **Step 3: Deploy to Vercel**

```bash
cd www && vercel --prod
```

- [ ] **Step 4: Create GitHub OAuth App**

Go to github.com/settings/developers → New OAuth App:
- Application name: sidekar
- Homepage URL: https://sidekar.dev
- Authorization callback URL: https://sidekar.dev/api/auth/github/callback

Copy Client ID and Client Secret into Vercel env vars.

- [ ] **Step 5: End-to-end test on production**

```bash
# 1. Visit sidekar.dev/sessions → redirects to GitHub login
# 2. Approve → see empty sessions dashboard
# 3. sidekar login → opens browser, enter code, approve
# 4. sidekar codex → tunnel connects
# 5. Refresh /sessions → see session
# 6. Click → terminal in browser
```

- [ ] **Step 6: Commit**

```bash
git add www/vercel.json
git commit -m "feat: Vercel routing for sessions, terminal, and approve pages"
```

---

## Implementation Order

Tasks are ordered by dependency:

1. **Task 1** — MongoDB schema (everything depends on this)
2. **Task 2** — GitHub OAuth (needed for browser auth)
3. **Task 3** — Device auth (needed for CLI auth)
4. **Task 4** — CLI auth module (needed for tunnel)
5. **Task 7** — Relay scaffold (needed before bridge)
6. **Task 8** — Relay auth (needed before bridge)
7. **Task 9** — Relay registry (needed before bridge)
8. **Task 10** — Relay bridge (core relay functionality)
9. **Task 5** — CLI tunnel client (needs relay running)
10. **Task 6** — PTY fan-out (needs tunnel client)
11. **Task 11** — Fly.io deployment (needs working relay)
12. **Task 12** — Sessions dashboard (needs relay API)
13. **Task 13** — Terminal page (needs relay + tunnel)
14. **Task 14** — Vercel deployment (ties it all together)

Tasks 2+3 can run in parallel. Tasks 7+8+9+10 are sequential but can overlap with Task 4. Tasks 12+13 can run in parallel.
