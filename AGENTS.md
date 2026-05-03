# Agent instructions (Sidekar repo)

Treat this file as binding for Cursor/Codex-style agents working in this repository. **`context/*.md` is not loaded automatically** — it is background for humans and for deep dives when cited. **If a procedure matters for correctness, it belongs here or in an executable script.**

## Local install (`cargo install` / PATH)

Do **not** hand-roll `cargo install` alone when the user wants a working `sidekar` on their PATH.

1. Run **`./scripts/install-local.sh`** from the repo root (symlinks `~/.local/bin/sidekar` → `~/.cargo/bin/sidekar`, runs macOS `xattr`/`codesign` when needed).
2. For rationale: `context/feedback_macos_binary_xattr.md` (Gatekeeper SIGKILL), release/signing notes in `context/todo.md` / `context/release-cycle.md`.

## Design docs under `context/`

Those files explain **why** code looks the way it does. Read the relevant file **when changing that subsystem** (comments in code often point to a path like `context/unified-exec.md`). They are **not** substitutes for `AGENTS.md` or scripts for operational steps.
