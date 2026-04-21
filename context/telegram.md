# Telegram integration

Treats a Telegram chat as a non-browser viewer of a sidekar session.
Inbound messages become keystrokes on the tunnel; outbound PTY bytes
are rendered as `sendMessage` calls. Zero changes to the sidekar CLI,
REPL, or PTY — everything lives in `relay/` + `www/`.

## Shape

```
Telegram user ─► @sidekar_bot ─► Telegram servers
                                       │ (webhook)
                                       ▼
                           POST /telegram/webhook
                      (X-Telegram-Bot-Api-Secret-Token
                      required and checked constant-time)
                                       │
                                       ▼
                         ┌──────────────────────────┐
                         │ TelegramState (in-mem)   │
                         │  chat_id → viewer task   │
                         └──────────────────────────┘
                                │             │
        inbound text            │             │   outbound PTY bytes
        (text or /command)      │             │   (via registry viewer
                                │             │    broadcast; ANSI-
                                ▼             ▼    stripped, chunked)
                    registry.push_tunnel_input   BotClient.send_message
                    (same path as WS viewers)
```

## Routes

- `POST /telegram/webhook` — Telegram pushes updates. Secret verified
  constant-time against `TELEGRAM_WEBHOOK_SECRET`. `update_id`
  deduped via `telegram_seen_updates` (unique index, 48h TTL). Always
  200s fast; work is spawned.
- `POST /telegram/deliver` — internal cross-relay hop. When a chat is
  bound to a session owned by another relay instance, the receiving
  instance forwards the inbound text here. Guarded by
  `RELAY_INTERNAL_SECRET`. Not exposed to Telegram or the browser.
- `GET  /telegram/link` — mints a link code (also available on the
  website as `/api/telegram/link` so the browser doesn't need to
  cross-origin call the relay).

## MongoDB collections

Created with `telegram::ensure_indexes()` at relay startup:

- `telegram_chats` — `{ chat_id (unique), user_id, session_id?,
  created_at, updated_at }`
- `telegram_link_codes` — `{ code (unique), user_id, created_at }`.
  One-shot, 10-min TTL, deleted on redeem.
- `telegram_seen_updates` — `{ update_id (unique), seen_at }`. 48h TTL.

## Bot commands

- `/start <code>` — link the chat to the user who generated the code.
- `/sessions` — list live sessions for the linked user.
- `/here <id>` — route messages to a specific session. If the session
  lives on this relay instance, the outbound viewer attaches here; if
  on another instance, inbound traffic will hop via `/telegram/deliver`
  when the user sends a message.
- `/stop` — unlink.
- `/help` / `/?` — usage.
- Any non-slash text → keystrokes on the routed session.

## Outbound rendering

The `TelegramViewer` task subscribes to the same `ViewerMsg` stream
the WebSocket viewer uses (via `registry.add_viewer`). It discards
the scrollback snapshot on attach — replaying full buffer history to
Telegram would be spam.

Stream handling:

- `ViewerMsg::Data(bytes)` → ANSI-strip, append to buffer, arm a
  1.2 s idle-flush timer. If buffer ≥ 3800 chars, flush immediately.
- `ViewerMsg::Control(json)` → if `ch:"events"` and the `event` field
  ends with `complete` / `done` / is `turn_end` / `assistant_message`,
  flush immediately. Other control frames are ignored (pty resize has
  no meaning on Telegram).

Rate limiting: per chat, minimum gap of 1.2 s between `sendMessage`
calls. Chunking preserves paragraph boundaries and is char-boundary
safe for multi-byte UTF-8.

## Secrets

- `TELEGRAM_BOT_TOKEN` — BotFather token. Required.
- `TELEGRAM_WEBHOOK_SECRET` — fixed string passed to `setWebhook` and
  checked on every incoming update. Required.
- `TELEGRAM_BOT_USERNAME` — public bot handle, no leading `@`.
  Optional; defaults to `sidekar_bot`. Used in the help text and deep
  link.
- `RELAY_INTERNAL_SECRET` — shared across relay instances so
  `/telegram/deliver` hops can be authenticated. Required only when
  running >1 relay instance.

All four live in GCP Secret Manager and are exported by the VM
startup script:

```
relay-telegram-bot-token
relay-telegram-webhook-secret
relay-telegram-bot-username   (optional)
relay-internal-secret
```

The website (Vercel) also reads `TELEGRAM_BOT_USERNAME` for the link
page so the "Send /start X to @botname" text matches.

## Deployment checklist

1. **Create bot with BotFather.** Save token → Secret Manager as
   `relay-telegram-bot-token`.
2. **Pick a webhook secret.** Any opaque string. Save as
   `relay-telegram-webhook-secret`.
3. **Update VM startup script.** Fetch both secrets, export as
   `TELEGRAM_BOT_TOKEN` / `TELEGRAM_WEBHOOK_SECRET` /
   `TELEGRAM_BOT_USERNAME` / `RELAY_INTERNAL_SECRET` in the relay
   container's env.
4. **Register webhook with Telegram:**

   ```bash
   curl -sS "https://api.telegram.org/bot${BOT_TOKEN}/setWebhook" \
     -d url=https://relay.sidekar.dev/telegram/webhook \
     -d secret_token=${WEBHOOK_SECRET} \
     -d "allowed_updates=[\"message\"]"
   ```

5. **Sanity-check:**

   ```bash
   curl -sS "https://api.telegram.org/bot${BOT_TOKEN}/getWebhookInfo"
   ```

   Expect `url` to match and `last_error_date` empty.

6. **Website env vars (Vercel):** add `TELEGRAM_BOT_USERNAME` so
   settings page shows the correct `@name`.

## Linking UX

Website `/settings` has a Telegram section that:

- Shows current bindings (chat_id, routed session).
- "Generate code" → `POST /api/telegram/link` → server mints an
  8-char code directly into MongoDB (Vercel and the relay share the
  DB, so no relay round-trip needed).
- Shows the code + a `https://t.me/<bot>?start=<code>` deep link.
- "Unlink all" → `POST /api/telegram/status?unlink=1`.

## Known limitations / next passes

- `/sessions` output isn't paginated; if a user has dozens of live
  sessions the reply may approach the 4096-char limit.
- Rendering is heuristic over raw PTY text. For long-running TUIs the
  idle-flush cadence may produce incoherent fragments; a `/raw`
  toggle that forwards unchanged per-line output would help.
- Per-chat viewer task runs on the relay that received the first
  local-route `/here`. If the user's session moves to another relay
  instance, the first inbound text after the move triggers a fresh
  attach via `/telegram/deliver`; outbound from the old instance
  keeps sending until the session heartbeat expires — a background
  reaper on `unregister()` would close it faster.
- No message editing / reaction support. Each chunk is an independent
  `sendMessage`.
