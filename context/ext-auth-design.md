# Extension Authentication Design

## Why the extension needs its own OAuth

The ext-server listens on localhost (e.g., `ws://127.0.0.1:9876`), but this doesn't make it secure by default. Any local process—including malicious extensions—can connect to a localhost WebSocket.

### Threat model

Without authentication, an attacker could:
1. Install a malicious Chrome extension
2. Scan localhost ports to find ext-server
3. Connect and send commands: `eval`, `read`, `screenshot`
4. Extract page content, credentials, session tokens

### How authentication prevents this

1. Extension obtains `ext_token` via GitHub OAuth (popup → sidekar.dev)
2. CLI has `device_token` from `sidekar login`
3. On WebSocket connect, extension sends `hello` with `ext_token`
4. ext-server calls `sidekar.dev/api/auth/ext-token?verify=1` with both tokens
5. Server verifies: `ext_token.user_id == device_token.user_id`
6. If mismatch → connection rejected

A malicious extension would get an `ext_token` for *their* GitHub account, not the victim's. The user ID check fails.

## Per-connection authentication

Authentication happens once per WebSocket connection, not per message:

```
Connect → authenticated = false
Send hello + ext_token → verify → authenticated = true
Subsequent messages → gated on authenticated flag
```

This is standard WebSocket auth. Re-verifying on every message would add latency and load.

Additional protections:
- Only one extension connection allowed at a time
- Connection state cleared on disconnect
- Native messaging restricts which extension can auto-launch ext-server

## Summary

The extension OAuth is not redundant—it prevents unauthorized local processes from hijacking the browser automation channel, even though ext-server only listens on localhost.
