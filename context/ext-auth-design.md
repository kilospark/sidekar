# Extension Authentication Design

## Why the extension needs its own OAuth

Native messaging restricts which Chrome extensions can launch Sidekar's local bridge, but that alone is not enough. The browser extension still needs to prove it belongs to the same sidekar.dev user as the CLI session it is trying to control.

### Threat model

Without authentication, an attacker could:
1. Install a malicious Chrome extension
2. Launch the native messaging host
3. Connect and send commands: `eval`, `read`, `screenshot`
4. Extract page content, credentials, session tokens

### How authentication prevents this

1. Extension obtains `ext_token` via GitHub OAuth (popup → sidekar.dev)
2. CLI has `device_token` from `sidekar device login`
3. On native bridge registration, extension sends `bridge_register` with `ext_token`
4. Native host calls `sidekar.dev/api/auth/ext-token?verify=1` with both tokens
5. Server verifies: `ext_token.user_id == device_token.user_id`
6. Native host registers the authenticated bridge with the daemon
7. If mismatch → bridge registration rejected

A malicious extension would get an `ext_token` for *their* GitHub account, not the victim's. The user ID check fails.

## Per-connection authentication

Authentication happens once per native messaging connection, not per message:

```
connectNative() → authenticated = false
Send bridge_register + ext_token → verify → authenticated = true
Subsequent commands → gated on authenticated bridge state
```

Re-verifying on every message would add latency and load.

Additional protections:
- Only one extension connection allowed at a time
- Connection state cleared on disconnect
- Native messaging manifest restricts which extension IDs may launch the host
- Daemon only forwards `sidekar ext ...` commands while the bridge is connected and authenticated

## Summary

The extension OAuth is not redundant. It binds the browser extension to the same sidekar.dev user as the CLI, even though the local bridge is launched through native messaging rather than a localhost WebSocket.
