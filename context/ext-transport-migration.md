# Extension Transport Migration

This note captures the browser-bridge architecture before and after the native-messaging cutover.

## Before

The old extension path used native messaging only for bootstrap. Actual command traffic ran over a localhost WebSocket.

```text
Chrome Extension
  |
  | native messaging: chrome.runtime.connectNative("dev.sidekar")
  v
sidekar native host
  |
  | ensure daemon/ext bridge is up
  | return port number
  v
localhost WebSocket bridge
  ws://127.0.0.1:9876
  |
  | persistent command channel
  v
sidekar daemon / ext bridge state
  |
  | unix socket RPC for CLI
  | ~/.sidekar/daemon.sock
  v
CLI: sidekar ext ...
```

There was also a separate localhost TCP IPC path for CLI control of the old ext-server flow:

```text
sidekar ext ...
  |
  | TCP IPC
  v
127.0.0.1:9877
  |
  v
ext-server
```

### Consequences of the old model

- Native messaging existed, but only to discover/bootstrap the bridge.
- Extension command traffic depended on localhost WebSocket state.
- The extension manifest needed localhost WS host permissions.
- The architecture carried separate concepts for:
  - native host bootstrap
  - localhost WS command channel
  - localhost TCP IPC for CLI/ext-server

## After

The current transport uses native messaging as the persistent extension bridge. The daemon remains the source of truth and the unix socket remains the local control plane.

```text
Chrome Extension
  |
  | persistent native messaging port
  | chrome.runtime.connectNative("dev.sidekar")
  v
sidekar native host
  |
  | verify ext_token against CLI device token
  | register bridge with daemon
  v
sidekar daemon
  ~/.sidekar/daemon.sock
  |
  | holds ext bridge state + pending requests
  v
CLI: sidekar ext ...
```

Current command flow:

```text
sidekar ext tabs
  -> daemon unix socket
  -> daemon ext bridge state
  -> native host
  -> extension
  -> browser command runs
  -> response returns over the same path
```

## What disappeared

- `ws://127.0.0.1:9876`
- `ws://localhost/*` extension host permissions
- `ws://127.0.0.1/*` extension host permissions
- localhost TCP IPC for ext-server control
- extension-side port discovery for browser commands

## What stayed the same

- `sidekar daemon` remains the long-lived owner of browser bridge state
- `~/.sidekar/daemon.sock` remains the local synchronous control interface
- SQLite remains the durable async bus and state store
- Extension auth still verifies:
  - extension `ext_token`
  - CLI `device_token`
  - same sidekar.dev user

## Why this cut was made

The old bridge added multiple local transport layers without delivering a corresponding product benefit for the current `sidekar ext` command surface. The native-messaging bridge removes:

- localhost port discovery
- localhost WebSocket lifecycle/reconnect state
- localhost WS permissions in the extension
- a major source of profile-specific bridge failures

while preserving the same user-facing `sidekar ext ...` capabilities.

## Relevant files

- `extension/background.js`
- `extension/manifest.json`
- `src/ext.rs`
- `src/daemon.rs`
- `context/ext-auth-design.md`
- `context/ext-auto-routing.md`
- `context/sidekar-daemon-design.md`
