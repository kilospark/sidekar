# OAuth Apps

Sidekar uses OAuth in three distinct contexts: **user authentication** (GitHub/Google sign-in for sidekar.dev accounts), **provider credentials** (Anthropic/Codex PKCE flows for LLM access in the REPL), and **workspace tools** (Google, Slack and Linear API access for `sidekar gmail|drive|…|slack|linear`, with apps the user registers).

## 1. User Authentication (sidekar.dev)

Users sign in to sidekar.dev via GitHub or Google OAuth. Both flows are handled by Vercel serverless functions under `www/api/auth/`.

### GitHub OAuth App

- **Owner:** kilospark organization
- **App ID:** 3480381
- **Client ID:** `Ov23lirUe7j4jwKco5kr`
- **Settings:** https://github.com/organizations/kilospark/settings/applications/3480381
- **Redirect URI:** `https://sidekar.dev/api/auth/github`
- **Scopes:** `read:user user:email`
- **Logo:** sidekar-icon-light-512.png
- **Handler:** `www/api/auth/github.js`
- **Env vars (Vercel):** `GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET`
- **Device Flow:** enabled (used by `sidekar login` on the consent screen)

### Google OAuth App

- **GCP Project:** `sidekar`
- **Client ID:** stored in Vercel env as `GOOGLE_CLIENT_ID`
- **Console:** https://console.cloud.google.com/auth/branding?project=sidekar
- **Redirect URI:** `https://sidekar.dev/api/auth/google`
- **Scopes:** `openid email profile`
- **Logo:** sidekar-icon-light-512.png
- **Handler:** `www/api/auth/google.js`
- **Env vars (Vercel):** `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET`

### Auth Flow

Both providers follow the same pattern:

1. User visits `/api/auth/{github,google}` (optionally with `?redirect=`)
2. Redirected to provider's authorization page
3. Provider redirects back with `code` to the same endpoint
4. Server exchanges code for access token, fetches user profile
5. Upserts user in MongoDB (`users` collection, keyed by `github_id` or `google_id`)
6. Issues a JWT session cookie, redirects to `returnTo`

Special `state` values:
- `link` / `link-mobile` — links the provider to an existing account (account linking)
- `mobile` — redirects to `sidekar://auth/callback?token=` for iOS app

### Device Auth (CLI login)

Separate from OAuth providers. The CLI uses a device-code flow:

1. `sidekar login` → POST `/api/auth/device` → gets `device_code` + `user_code`
2. User opens `https://sidekar.dev/approve` in browser, enters code
3. CLI polls `/api/auth/device?action=token` until approved
4. Server returns a device token (stored locally in SQLite)

The user must be signed into sidekar.dev (via GitHub or Google) to approve the device code.

### Extension Auth

The Chrome extension authenticates separately via `ext_token` (see `context/ext-auth-design.md`). On native bridge registration, the server verifies that the extension's `ext_token` and the CLI's `device_token` belong to the same user.

## 2. Provider Credentials (LLM access)

The REPL's `sidekar cred` command uses PKCE OAuth to obtain API tokens from LLM providers. These are stored encrypted in the KV store.

### Anthropic (Claude)

- **Client ID:** `9d1c250a-e61b-44d9-88ed-5944d1962f5e`
- **Authorize URL:** `https://claude.com/cai/oauth/authorize`
- **Token URL:** `https://platform.claude.com/v1/oauth/token`
- **Callback port:** 53692
- **KV key:** `oauth:anthropic` (or `oauth:claude-<name>` for named creds)

### Codex (OpenAI)

- **Client ID:** `app_EMoamEEZ73f0CkXaXp7hrann`
- **Authorize URL:** `https://auth.openai.com/oauth/authorize`
- **Token URL:** `https://auth.openai.com/oauth/token`
- **Callback port:** 1455
- **KV key:** `oauth:codex` (or `oauth:codex-<name>`)

### Flow

1. `sidekar cred add anthropic` (or `codex`)
2. Opens browser to provider's authorize URL with PKCE challenge
3. Localhost callback server receives the code
4. Exchanges code for access + refresh tokens
5. Stores `OAuthCredentials` (encrypted) in KV as `oauth:<provider>`
6. Auto-refreshes expired tokens before use

Handler: `src/providers/oauth.rs`

## 3. Workspace tools (Google, Slack, Linear)

No sidekar-owned app and no relay involvement: the user creates their own OAuth
app in each service, stores its client id/secret in kv under keys they name,
and `sidekar <svc> login` runs the consent flow against a loopback listener on
this machine (`src/oauth_loopback.rs`). Tokens are stored in kv under a key the
user names, tagged with what minted them, so several accounts can coexist.

| Service | Redirect | How it matches | Token stored | Handler |
|---|---|---|---|---|
| Google | `http://127.0.0.1:<random>` | Desktop client accepts any loopback port | refresh token | `src/google/auth.rs` |
| Slack | `http://localhost:53694/callback` (`--port`) | host+port must match a registered Redirect URL | user (`xoxp-`) or bot (`xoxb-`) token; JSON blob with refresh token if rotation is on | `src/slack/auth.rs` |
| Linear | `http://localhost:53695/callback` (`--port`) | exact match against the app's Callback URLs | JSON blob: 24h access token + rotating refresh token + expiry | `src/linear/auth.rs` |

Without an app: `sidekar slack add` adopts an `xoxp-`/`xoxb-` token already in
kv, and `sidekar linear add` adopts a personal API key (`lin_api_…`, sent bare in
`Authorization`; OAuth tokens go as `Bearer`).

Slack scopes (user): `channels:read groups:read im:read mpim:read channels:history
groups:history im:history mpim:history chat:write im:write users:read
users:read.email search:read bookmarks:read files:read files:write`. Bot: the same minus `search:read`,
which Slack does not offer bots. `sidekar slack setup` prints a ready app manifest.
Tokens issued before `bookmarks:read` and `files:read`/`files:write` were added
need the app reinstalled (or `slack login` again) before `slack bookmarks`,
`file`, `download`, `upload` and `--attach` work. Without `files:read` Slack
answers a file URL with its sign-in page; `slack download` reports that as the
missing scope.

The loopback listener (`wait_for_code`) puts a 3 s read deadline on every
connection and keeps listening past anything that is not the redirect (an idle
connection, a favicon request, a short read), so only the overall 5 minute
consent timeout ends the wait. `state` is 128 random bits and is checked before
anything else in the redirect: a request with a missing or wrong state gets a
404 and the listener keeps waiting, so neither a stray request nor a forged
`?error=` from another local process can end or cancel the login. Both
loopback addresses must be free; a taken `[::1]` port is an error, not
something to skip (only a machine without IPv6 is skipped).

PKCE (S256): Google and Linear always get a `code_challenge`, and the exchange
sends the `code_verifier` with the client secret. Slack only accepts PKCE once
the app has PKCE turned on, which is permanent and makes the app a public client
(no secret, and no bot scopes on a localhost redirect), so it is opt-in:
`slack login --pkce` sends the challenge and exchanges with the verifier
instead of the secret, and refuses `--bot`.

`slack login --team T…` checks the workspace the token came back for (by id or
name) and stores nothing if it is a different one.

Refresh: rotating refresh tokens (Linear always; Slack with rotation on) are
spent once, so read → refresh → write is serialised by a file lock per kv key
(`~/.sidekar/locks/token-<key>.lock`). After taking the lock the entry is read
again, so a caller that waited uses the token the first one wrote instead of
spending the old refresh token. When a refresh fails, the entry is read once
more before reporting it: a valid token written meanwhile (by another process,
or by kv sync from another machine) is used, and a newer refresh token gets one
retry. Each rotation clears the key's kv history, so spent tokens are not kept
as old versions. The lock covers this machine only; two machines refreshing in
the same moment can still race, and the loser's error says to log in again.

Slack name lookups (`#name`, a handle or display name, user ids shown as names)
search the person's own channels (`users.conversations`) before the whole
workspace, stop at the first exact match, and cache name → id for 15 minutes in
`~/.sidekar/cache/slack-names-<team>.json`. Names for user ids are fetched eight
at a time.

`slack draft` calls `drafts.create`, which is not in Slack's published API. It
takes an OAuth user token with no extra scope; bot tokens are refused. Drafting
to a person opens your DM with them via `conversations.open` (`im:write`) when
there is none yet: Slack shows them nothing until a message is sent, but the
empty DM exists afterwards. `--existing-dm` refuses instead of opening one. `drafts.list`/`update`/`delete` return
errors for OAuth tokens, and Saved for later (`saved.list`) needs a browser
session token (`xoxc`), so neither is offered. `stars.list` is legacy and
reflects the retired Stars feature, not Saved for later.

Linear scopes: `read,write` (enough for `fileUpload`, `attachmentCreate` and
`attachmentLinkURL`; files on `uploads.linear.app` are fetched with the same
`Authorization` header, sent to that host only). Linear rotates the refresh token on every use, so
the access token is cached in the blob and only refreshed within five minutes of
expiry; each refresh rewrites the kv entry (which syncs) under the lock above.

Linear lists follow `pageInfo { hasNextPage endCursor }`: label, project and
cycle lookups read every page, so a name past the first 250 still resolves, and
listings (`issues`, `projects`, `users`, `cycles`, `inbox`, `activity`) end with
a note when `--limit` cut them.

## Database Collections

All in MongoDB Atlas (`sidekar` database):

| Collection | Purpose |
|---|---|
| `users` | User accounts. Keyed by `github_id` and/or `google_id` |
| `device_codes` | Pending device authorization codes (TTL: 15min) |
| `devices` | Registered CLI devices (token hash, hostname, OS, version) |
| `ext_tokens` | Chrome extension tokens (max 10 per user) |
