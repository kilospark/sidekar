# Sidekar Chrome extension

MV3 extension that connects to the local `sidekar` bridge so terminal agents can control **your everyday Chrome** (same profile, cookies, and logins as the window you already use).

## Install

1. Build or install the `sidekar` binary (`cargo build --release` from the repo root).
2. Log in: `sidekar login` (this also installs native messaging for the extension).
3. Chrome → **Extensions** → enable **Developer mode** → **Load unpacked** → select this `extension/` directory.
4. Click the Sidekar toolbar icon → **Login with GitHub**.

The extension auto-discovers the local bridge via native messaging—no port configuration needed.

## Terminal usage

From any shell, with Chrome running and the extension connected:

```bash
sidekar ext tabs
sidekar ext read
sidekar ext read 42              # specific tab id
sidekar --tab 42 ext screenshot  # same, via global flag
sidekar ext click '#submit'
sidekar ext paste --html '<h1>Title</h1>' --text 'Title'
sidekar ext setvalue '.monaco-editor' 'const x = 1;'
sidekar ext evalpage 'window.monaco?.editor?.getEditors?.()[0]?.getValue()'
sidekar ext status
```

The bridge starts automatically when needed.

## Files

| File | Purpose |
|------|---------|
| `manifest.json` | MV3 manifest, permissions |
| `background.js` | WebSocket client, command handlers |
| `popup.html` / `popup.js` | OAuth login and connection status |
| `icons/icon-{16,48,128}.png` | Toolbar icons |
| `generate_icons.py` | Regenerate PNGs (`pip install pillow`, `python3 generate_icons.py`) |
