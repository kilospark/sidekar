# Agent instructions (Sidekar repo)

Treat this file as binding for Cursor/Codex-style agents working in this repository. **`context/*.md` is not loaded automatically** — it is background for humans and for deep dives when cited. **If a procedure matters for correctness, it belongs here or in an executable script.**

## Local install (`cargo install` / PATH)

Do **not** hand-roll `cargo install` alone when the user wants a working `sidekar` on their PATH.

1. Run **`./scripts/install-local.sh`** from the repo root (symlinks `~/.local/bin/sidekar` → `~/.cargo/bin/sidekar`, runs macOS `xattr`/`codesign` when needed).
2. For rationale: `context/feedback_macos_binary_xattr.md` (Gatekeeper SIGKILL), release/signing notes in `context/todo.md` / `context/release-cycle.md`.

## Design docs under `context/`

Those files explain **why** code looks the way it does. Read the relevant file **when changing that subsystem** (comments in code often point to a path like `context/unified-exec.md`). They are **not** substitutes for `AGENTS.md` or scripts for operational steps.

## REPL terminal I/O vs web relay (`tunnel_*`)

Anything the user sees or types inside **`sidekar repl`** while **`/relay` may be on** must stay symmetric between the local terminal and relay viewers:

- **Never** emit interactive REPL/transcript-facing text with raw `print!` / `println!` / `stdout.write` that bypass `crate::tunnel::tunnel_print` (partial lines, prompts) or `crate::tunnel::tunnel_println` (full lines). When no relay is registered, those helpers degrade to ordinary local stdout.
- **Never** answer REPL prompts with bare `stdin().read_line` when the relay input FD exists: use `crate::repl::editor::read_line_stdio_or_tunnel` (same stdin + tunnel FD routing as the main read loop). Slash menus, credential add (`InteractiveOutput::Repl`), interactive model picker, etc. thread `tunnel_input_fd` through for this reason.
- **`crate::tunnel` is canonical** — do not reimplement “write stdout + optionally mirror relay” beside it.
- **Out of scope** (plain CLIs without relay/session bridge unless you extend them): device login prompts in `src/auth.rs`, skill installer output in `src/skill.rs`, install/uninstall/update buffered output in `src/main.rs`.
