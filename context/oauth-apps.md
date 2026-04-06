# OAuth Apps

Sidekar uses OAuth in two distinct contexts: **user authentication** (GitHub/Google sign-in for sidekar.dev accounts) and **provider credentials** (Anthropic/Codex PKCE flows for LLM access in the REPL).

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

## Database Collections

All in MongoDB Atlas (`sidekar` database):

| Collection | Purpose |
|---|---|
| `users` | User accounts. Keyed by `github_id` and/or `google_id` |
| `device_codes` | Pending device authorization codes (TTL: 15min) |
| `devices` | Registered CLI devices (token hash, hostname, OS, version) |
| `ext_tokens` | Chrome extension tokens (max 10 per user) |
