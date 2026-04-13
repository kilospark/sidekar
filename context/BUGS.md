# REPL Bugs

Source-backed findings from code inspection in the REPL implementation. These are not end-to-end repro notes yet.

## 1. Session `provider` field is populated with non-provider values

- The schema stores a `provider` string on REPL sessions in [`src/session.rs`](./src/session.rs).
- REPL writes `"oneshot"` in prompt mode, `"repl"` in the normal interactive path, and `"anthropic"` for `/new` and picker-init.
- Result: the `provider` field is not reliably a provider at all.

Relevant source:
- `src/session.rs:18`
- `src/session.rs:74`
- `src/repl.rs:140`
- `src/repl.rs:202`
- `src/repl.rs:319`
- `src/repl.rs:687`

## 2. `-p` prompt mode ignores resume semantics

- The parser accepts resume flags.
- `run_with_options` checks prompt mode first and unconditionally creates a fresh `"oneshot"` session.
- Result: when a user passes both `-p` and `-r`, the resume request is ignored.

Relevant source:
- `src/main.rs:404`
- `src/main.rs:408`
- `src/repl.rs:138`
