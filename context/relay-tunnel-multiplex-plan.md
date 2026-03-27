# Relay tunnel multiplexing — plan & status

## Goal

Use the **existing PTY WebSocket tunnel** (`/tunnel`) to carry a second logical channel (**bus**) between machines for the same **sidekar.dev user**, without breaking the web terminal (PTY binary path).

## Wire format (v1)

| Direction | PTY | Bus (multiplex) |
|-----------|-----|-----------------|
| Client → relay | **Binary** (unchanged) | **Text** JSON: `{"ch":"bus","v":1,"from_session":"<uuid>","body":"<text>"}` |
| Relay → client | **Binary** | **Text** same schema (forwarded from other tunnels) |
| Relay → web viewer | **Binary** only | Bus **not** sent to browser terminal in v1 |

- **Legacy clients:** omit `proto` or `proto: 1` → only binary PTY; relay does not forward bus.
- **Multiplex clients:** `proto: 2` in the initial `register` JSON → `LiveSession.multiplex = true`.

## Components

### Relay (`relay/`)

1. **`RegisterMsg`:** optional `proto: u8` (default 1).
2. **`LiveSession`:** `multiplex: bool` (`proto >= 2`).
3. **`TunnelMsg`:** `Data(Vec<u8>)` | `Text(String)` — text for bus to/from WebSocket.
4. **`Registry::forward_bus_to_peers(from_session_id, text)`** — same `user_id`, other sessions with `multiplex`, not self.
5. **`handle_tunnel_socket`:** on **Text** from tunnel, if valid bus JSON (`ch == "bus"`), call `forward_bus_to_peers`. Binary unchanged → `broadcast_to_viewers`.
6. **Main loop:** on `TunnelMsg::Text`, send `Message::Text` to tunnel WebSocket.

### Sidecar (`src/tunnel.rs`)

1. **`RegisterMsg`:** `proto: 2` in register.
2. **`TunnelCommand::BusText(String)`** — full JSON string to send as Text.
3. **`TunnelEvent::BusRelay` / `BusPlain`** — routed (`recipient`/`sender`/`body`) vs legacy body-only.
4. **`TunnelSender::send_bus_routed(recipient, sender, body)`** — multiplex JSON including `from_session`.
5. **`io_loop`:** on `Message::Text` from relay, parse bus → `BusRelay` or `BusPlain`.

### PTY (`src/pty.rs`)

- **`BusRelay`:** if `recipient` matches this agent, `broker::enqueue_message` (poller delivers).
- **`BusPlain`:** write `body + "\r\n"` to PTY master.

## Backward compatibility

- Old relay + new client: extra `proto` field ignored by serde if not in struct — **relay must accept unknown fields** (no `deny_unknown_fields`). New relay reads `proto`.
- New relay + old client: no bus; binary-only — OK.
- Web terminal: only receives binary PTY — unchanged.

## Follow-ups

- [x] **`bus_send` → remote peers:** `find_delivery_target` falls back to **`relay_http`** when `GET /sessions` (Bearer device token) lists a live session whose `name` or `nickname` matches; delivery is **`POST /relay/bus`**; recipients handle **`TunnelEvent::BusRelay`** by **`broker::enqueue_message`** (canonical agent `name` in `recipient`).
- [ ] **Rate limits** / max body size on bus JSON (relay `POST /relay/bus`).
- [ ] **Optional:** bus to web UI (separate viewer channel).

### Wire note (routed bus)

Tunnel + HTTP use the same JSON shape: `ch: "bus"`, `v: 1`, **`recipient`**, **`sender`**, **`body`**. Receivers enqueue only when `recipient` equals their registered agent name.

## Resume checklist if interrupted

- [x] `relay/src/types.rs` — `proto` on `RegisterMsg`
- [x] `relay/src/registry.rs` — `TunnelMsg`, `multiplex`, `forward_bus_to_peers`
- [x] `relay/src/bridge.rs` — register flag, Text in, `TunnelMsg::Text` out
- [x] `src/tunnel.rs` — proto 2, bus send/recv
- [x] `src/pty.rs` — handle `TunnelEvent::BusRelay` / `BusPlain`
- [x] `cargo check` in repo root and `relay/`
