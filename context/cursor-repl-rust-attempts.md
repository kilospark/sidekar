# Cursor REPL — Native Rust Attempts & What We Abandoned

**Date:** 2026-05-03  
**Audience:** Humans picking this up later; procedures stay in code (`src/providers/cursor.rs` module docs) or `AGENTS.md`.

Sidekar already runs Cursor well in **PTY mode** (`CursorFamily`, `src/agent_cli/cursor_family.rs`) by wrapping Cursor’s CLI. That path is unaffected.

This note is specifically about exposing **Cursor as a Sidekar REPL “provider”** using **Rust + the same HTTPS backend MitM/logging already shows**, without shipping a brittle or unwanted integration style.

---

## What we wanted

- **Rust only** in the REPL stack: no separate Node process, no delegating chat to `@cursor/sdk` as a subprocess bridge.
- **Not** Cursor **Cloud Agents** (`https://api.cursor.com`, GitHub repo wiring, cloud-agent REST shape). That is a different product surface from the desktop/SDK traffic many captures show on **`https://api2.cursor.sh`** with **Connect-style RPC paths** and protobuf bodies.
- **Ground truth** from **proxy / MITM logs** (and, secondarily, dissected JS bundles with protobuf field metadata), not inventing message layouts.

---

## Attempts we tried and abandoned

### 1. Cloud Agents HTTP API

**Idea:** Treat Cursor like other vendors with a documented REST surface under `api.cursor.com` (agents, models, etc.).

**Abandoned because:** It diverges from the goal above: different auth model, different semantics, ties to cloud-agent workflows the maintainer explicitly does not want. Any code or copy that described “log in for cloud agents” was removed or corrected in favor of the **`api2.cursor.sh` / exchange + Connect** story.

### 2. Node / `@cursor/sdk` bridge

**Idea:** Keep Sidekar’s REPL in Rust but spawn or embed Node to run the official SDK for streaming.

**Abandoned because:** Same rejection as “no Node”: the desired architecture is a **native Rust wire client**, not a second runtime acting as the real client.

**Removed from tree (historical):** a local bridge layout under `assets/` and a `cursor_local`–style provider module were deleted; `.gitignore` entries tied only to that bridge were dropped. The REPL should route Cursor through **`Provider::Cursor` → `src/providers/cursor.rs`** only.

### 3. Full streaming without checked-in protos

**Idea:** Fake or partially guess `agent.v1.AgentService` message shapes (or older `aiserver.v1` chat RPCs) and hope the server accepts them.

**Not pursued:** Sending guessed protobuf on a live session is how you get opaque 4xx failures and silent breakage every Cursor release. Streaming chat needs **accurate `prost` messages** aligned to what **`Run`** (bidirectional streaming) and follow-on RPCs expect—including details like **`BidiRequestId`** coming from **`Run`**, not a random UUID for **`RunSSE`** / **`RunPoll`**.

Earlier research (`context/cursor-agent-api.md`) cataloged **`aiserver.v1.AiService` / `StreamChat`** fields from minified JS. Newer bundles and captures also reference **`agent.v1.AgentService`** with **`Run`**, **`RunSSE`**, **`RunPoll`**, etc.; treat those as overlapping or evolved layers until one capture path is chosen and frozen in protos.

---

## What remains in the repo (intentional partial)

As of this writing:

- **`src/providers/cursor.rs`** — Pure Rust toward **`CURSOR_BACKEND_URL` / `CURSOR_API_BASE_URL`** (default **`https://api2.cursor.sh`**):
  - **`POST /auth/exchange_user_api_key`** with `Authorization: Bearer <CURSOR_API_KEY>` body `{}` → JWT **`accessToken`**.
  - Optional probe: **`SIDEKAR_CURSOR_CONNECT_PROBE=1`** → unary **`AgentService/GetUsableModels`** over **`application/connect+proto`** unary framing.
  - **`stream`**: emits a clear error (and terminates the turn) until bidirectional **`Run`** + **`AgentClientMessage` / `AgentServerMessage`** (or whichever RPC set we commit to) are implemented from **real captures**, not guesses.

- **Credential / UX copy** aligned with backend exchange, not cloud agents (`oauth.rs`, `credential_login.rs`, provider docs in `mod.rs`).

- **MitM helpers** (`build_streaming_client`, etc.) so proxy-attached Sidekar sees the same TLS trust as other providers — no Cursor-specific Node bridge in comments or architecture.

---

## If someone reopens full REPL Cursor later

Minimal checklist:

1. **Freeze wire choice:** `AiService.StreamChat`-style unary server-stream vs `agent.v1.AgentService.Run` BiDi — from **your** latest `proxy_log` / PCAP, not from this doc alone.
2. **Export binaries** from `sidekar proxy show …` (or equivalent) for one successful IDE/SDK turn.
3. **Recover `.proto`** or handwritten `prost` structs from field numbers + lengths; codegen and keep a small conformance test round-trip against saved frames.
4. **Map REPL** `ChatMessage[]` / tools into Cursor’s structs (almost certainly non–1–1 with Sidekar’s generic tool graph).

Until then, **PTY mode stays the reliable Cursor integration**; Rust REPL is deliberately **credential + exchange + connectivity probe**, not full chat.

---

## Related

- Older protocol notes and `StreamChat`-oriented protobuf tables: `context/cursor-agent-api.md`
