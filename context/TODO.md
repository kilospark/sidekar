# TODO

## High Priority

- [ ] Terminal adapters for unmanaged sessions (Terminal.app, iTerm2, Warp, kitty, Ghostty)
- [ ] Editor adapters (VS Code, Cursor, Zed)
- [ ] Desktop app adapters (Codex app, Claude desktop)
- [ ] Clarify and harden first-install signature verification path (`install.sh` bootstrap trust / how signatures are checked before Sidekar is already installed)
- [ ] Publish Chrome extension to Web Store
- [ ] Update website copy

## Medium Priority

- [ ] Google login (in addition to GitHub)
- [ ] Attention/notification system (desktop notifications, terminal badges)
- [ ] Task queue visibility and claiming UX
- [ ] Session inspection tools (`sidekar sessions`, `sidekar attach`)
- [ ] Test multiple relay machines in Fly.io
- [ ] Test GCP for relay
- [ ] Refactor/security review
- [ ] Add nairo/memory integration
- [ ] Define `nairo` scope model: project-level vs user-level

## Low Priority / Future

- [ ] A2A gateway for external agent interop
- [ ] Linux support
- [ ] Windows support

## Recently Completed

- [x] Daemon consolidation (ext-server absorbed into daemon)
- [x] Chrome extension OAuth flow
- [x] Native messaging for extension auto-connect
- [x] Cross-channel bus messaging
- [x] KV/TOTP encryption at rest
- [x] `sidekar devices` and `sidekar sessions` commands
- [x] `--verbose` flag for startup messages
- [x] Suppress Chrome automation infobar (`--test-type`)
- [x] Bus warning when not in sidekar wrapper
