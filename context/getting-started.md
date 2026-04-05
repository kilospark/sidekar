# Getting started (step by step)

1. **Install:** `curl -fsSL https://sidekar.dev/install | sh` installs the binary and runs `sidekar install` to place `SKILL.md` into each detected agent’s skills directory.

2. **`sidekar device login` (optional):** Run `sidekar device login` to authenticate this machine with sidekar.dev. This enables relay-backed remote session access, the web terminal, account session management, and account-backed encryption state.

3. **Chrome extension (optional):** Load unpacked from the `extension/` directory (Chrome → Extensions → Developer mode → Load unpacked), then click **Login with GitHub** in the popup. The bridge starts automatically. See `extension/README.md`.

4. **Choose an access mode:**
   - `sidekar <agent> [args…]` wraps an external agent CLI in a PTY with bus registration, browser integration, and relay support.
   - `sidekar repl -c <credential> -m <model>` runs Sidekar's built-in standalone LLM REPL.

For capability overview, see `context/feature-pillars.md`.
