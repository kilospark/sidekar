# Getting started (step by step)

1. **Install:** `curl -fsSL https://sidekar.dev/install | sh` installs the binary and runs `sidekar install` to place `SKILL.md` into each detected agent’s skills directory.

2. **Chrome extension (optional):** Load unpacked from the `extension/` directory (Chrome → Extensions → Developer mode → Load unpacked). Start the bridge with `sidekar ext-server` or any `sidekar ext …` command, paste the shared secret in the extension popup, and connect. See `extension/README.md`.

3. **`sidekar login` (optional):** Run `sidekar login` to authenticate with sidekar.dev and store a device token when you want remote session access (web dashboard, tunnel workflows), not only local use.

4. **Launch an agent:** `sidekar <agent> [args…]` where `<agent>` is any agent CLI on your `PATH` or a shell alias (for example `sidekar claude`, `sidekar codex`). Sidekar runs the agent in a PTY with bus registration and browser integration.

For capability overview, see `context/feature-pillars.md`.
