# Getting started (step by step)

1. **Install:** `curl -fsSL https://sidekar.dev/install | sh` installs the binary and runs `sidekar install` to place `SKILL.md` into each detected agent’s skills directory.

2. **`sidekar login`:** Run `sidekar login` to authenticate with sidekar.dev. This enables remote session access (web dashboard, tunnel) and sets up native messaging for the Chrome extension.

3. **Chrome extension (optional):** Load unpacked from the `extension/` directory (Chrome → Extensions → Developer mode → Load unpacked), then click **Login with GitHub** in the popup. The bridge starts automatically. See `extension/README.md`.

4. **Launch an agent:** `sidekar <agent> [args…]` where `<agent>` is any agent CLI on your `PATH` or a shell alias (for example `sidekar claude`, `sidekar codex`). Sidekar runs the agent in a PTY with bus registration and browser integration.

For capability overview, see `context/feature-pillars.md`.
