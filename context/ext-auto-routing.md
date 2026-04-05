# Extension auto-routing design

## Current state

CLI has two explicit paths:
- `sidekar read` → CDP-launched Chrome (isolated profile)
- `sidekar ext read` → extension bridge to user's real Chrome

Agent must manually choose. No auto-detection.

## Extension distribution: dev-extract

For development/testing before Chrome Web Store publication, the extension ZIP is embedded in the binary:

```bash
sidekar ext dev-extract   # Extract to ~/.sidekar/extension
```

- ZIP embedded via `include_bytes!` in `src/ext.rs`
- Extraction target: `~/.sidekar/extension`
- Always overwrites on each run (supports updating extension)
- Load via Chrome: Extensions → Enable Developer → Load unpacked → select `~/.sidekar/extension`
- Build: `./local-release.sh` automatically zips `extension/` to `assets/extension.zip` before building

## Proposed behavior

CLI auto-detects if the Chrome extension is connected and routes through it by default. No more `sidekar ext` prefix for normal use.

### Priority chain

1. **Explicit profile** (`sidekar launch --profile X`) → always CDP, isolated
2. **Extension connected** → use it (user's real Chrome, tab isolation enforced)
3. **No extension** → fall back to CDP with default profile

### Why this is safe

- Tab isolation already enforced: agent only reads/touches tabs it opened via `new-tab`/`navigate`
- `close` only closes agent-owned tabs, never pre-existing ones
- Users who install the extension are knowingly opting into real-browser control
- Explicit `--profile` flag still gives full isolation when needed

### Implementation notes

- Add `pub fn is_ext_available() -> bool` to `ext.rs` — checks daemon running + authenticated extension bridge (`ext_status`)
- In main dispatch: before auto-launching CDP, check `is_ext_available()`. If true, route the command through the extension bridge instead
- `sidekar ext` prefix still works as explicit override
- `sidekar launch` still spins up CDP when called directly

### Extension auth redesign (related)

Replace shared secret with OAuth:
1. User installs extension, clicks icon → popup shows "Login with GitHub"
2. Extension opens `sidekar.dev/api/auth/github?redirect=ext-callback`
3. OAuth completes, server returns token, extension stores in `chrome.storage.local`
4. On native bridge registration, extension sends token to the native host
5. Native host verifies both tokens (extension + CLI from `sidekar device login`) belong to same user
6. Match → daemon bridge registered. Mismatch → rejected.

This requires `sidekar device login` for extension use, which is fine — extension is a power-user feature. Eliminates the manual secret copy-paste step entirely.
