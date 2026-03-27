# Sidekar

Browser automation via the Chrome DevTools Protocol. Rust implementation.

## Binary

The `sidekar` binary is built from `src/`.

## Context folder

Read **`context/`** before making substantive changes or answering product- or repo-specific questions. It holds contributor-oriented notes: getting started, feature pillars, two meanings of “session” (bus vs Chrome/CDP), macOS local binary xattr/signing after `cp`, Vercel/Fly deployment, and design drafts. Start with `context/getting-started.md` and `context/feature-pillars.md`.

## Working principles

- **Do not guess, assume, or invent.** If you are not sure, say so.
- **Ground claims in this repo:** the code, `context/`, `README.md`, and other checked-in docs—not speculation or generic “usually” behavior.
- **Prefer authoritative sources** (official APIs, this codebase, user-provided facts) over plausible-sounding filler.
- **If something is unclear or under-specified, stop and ask the user** instead of proceeding on a guess.

## Setup

Install via `install.sh` or build from source with `cargo build --release`.

## Sandbox Note

The CDP tool launches Chrome on an automatically discovered free port. The port is printed in the launch output and saved in the session state. If your agent sandbox blocks local network access, you'll need to allow connections to `127.0.0.1` on the assigned port.
