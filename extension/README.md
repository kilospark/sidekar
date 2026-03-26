# Sidekar Chrome extension

MV3 extension that connects to the local `sidekar` bridge so terminal agents can control **your everyday Chrome** (same profile, cookies, and logins as the window you already use).

## Install

1. Build or install the `sidekar` binary (`cargo build --release` from the repo root).
2. Chrome → **Extensions** → enable **Developer mode** → **Load unpacked** → select this `extension/` directory.
3. Start the bridge and copy the shared secret:
   - `sidekar ext secret`
4. Click the Sidekar toolbar icon → paste the secret → **Connect**.

Default WebSocket URL is `ws://127.0.0.1:9876`. If you use another port on the Rust side (`SIDEKAR_EXT_PORT` / `sidekar ext-server`), set the same value as **Bridge port** in the extension popup (stored in `chrome.storage.local`).

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

The first `sidekar ext …` command starts `ext-server` in the background if it is not already running (logs under `~/.sidekar/ext-server.log`).

**Popup stays on "Not connected"** — the extension does not start the Rust process; it only connects to WebSockets. Start the bridge first, e.g. `sidekar ext tabs` or `sidekar ext-server`, then click Connect. If the secret fails, run `sidekar ext secret` again and paste with no extra spaces. The popup shows a short error line when the socket cannot connect or auth fails.

## Files

| File | Purpose |
|------|---------|
| `manifest.json` | MV3 manifest, permissions |
| `background.js` | WebSocket client, command handlers |
| `popup.html` / `popup.js` | Secret entry and connection status |
| `icons/icon-{16,48,128}.png` | Toolbar icons (default: light asset `sidekar-icon-light-512.png`; Chrome UI is light) |
| `generate_icons.py` | Regenerate PNGs (`pip install pillow`, `python3 generate_icons.py`; `SIDEKAR_EXT_ICON=dark` for dark) |
