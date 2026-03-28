# Sidekar Chrome extension

MV3 extension that connects to the local `sidekar` bridge so terminal agents can control **your everyday Chrome** (same profile, cookies, and logins as the window you already use).

## Install

1. Build or install the `sidekar` binary (`cargo build --release` from the repo root).
2. Log in to sidekar.dev: `sidekar login`
3. Chrome → **Extensions** → enable **Developer mode** → **Load unpacked** → select this `extension/` directory.
4. Click the Sidekar toolbar icon → **Login with GitHub**.

Default WebSocket URL is `ws://127.0.0.1:9876`. If you use another port on the Rust side (`SIDEKAR_EXT_PORT` / `sidekar ext-server`), set the same value as **Bridge port** in the extension popup.

## Terminal usage

From any shell, with Chrome running and the extension connected:

```bash
sidekar ext tabs
sidekar ext read
sidekar ext read 42              # specific tab id
sidekar --tab 42 ext screenshot  # same, via global flag
sidekar ext click '#submit'
sidekar ext status
```

The first `sidekar ext …` command starts `ext-server` in the background if it is not already running.

**Popup stays on "Not connected"** — the extension does not start the Rust process; it only connects to WebSockets. Start the bridge first (`sidekar ext tabs` or `sidekar ext-server`), then the extension will auto-connect. The popup shows an error when the socket cannot connect or auth fails.

## Files

| File | Purpose |
|------|---------|
| `manifest.json` | MV3 manifest, permissions |
| `background.js` | WebSocket client, command handlers |
| `popup.html` / `popup.js` | OAuth login and connection status |
| `icons/icon-{16,48,128}.png` | Toolbar icons |
| `generate_icons.py` | Regenerate PNGs (`pip install pillow`, `python3 generate_icons.py`) |
