# REPL Bugs

Source-backed findings from code inspection in the REPL implementation. These are not end-to-end repro notes yet.

## 1. Named `repl login <nickname>` is not guaranteed to store under that nickname

- `sidekar repl login <nickname>` deletes only `oauth:<nickname>` before login in [`src/main.rs`](./src/main.rs).
- The login helpers call `resolve_kv_key`, which falls back to the default provider key if it already exists in [`src/providers/oauth.rs`](./src/providers/oauth.rs).
- Result: `sidekar repl login claude-work` can succeed using `oauth:anthropic` without creating `oauth:claude-work`.

Relevant source:
- `src/main.rs:195`
- `src/providers/oauth.rs:34`
- `src/providers/oauth.rs:128`
- `src/providers/oauth.rs:168`

## 2. Session `provider` field is populated with non-provider values

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

## 3. `-p` prompt mode ignores resume semantics

- The parser accepts resume flags.
- `run_with_options` checks prompt mode first and unconditionally creates a fresh `"oneshot"` session.
- Result: when a user passes both `-p` and `-r`, the resume request is ignored.

Relevant source:
- `src/main.rs:404`
- `src/main.rs:408`
- `src/repl.rs:138`

## 4. Top-level help is out of sync with the actual REPL parser

- `sidekar --help` shows only `[-c cred] [-m model] [-r session]` for REPL usage.
- The parser actually supports `-p`, bare `--resume`, `--resume-session`, `--credential`, and `-v/--verbose`.
- The command-specific help is closer to reality than the top-level help.

Relevant source:
- `src/cli.rs:1040`
- `src/main.rs:386`
- `src/lib.rs:2926`

## 5. Provider-name matching is broader than the comment implies

- The comment says OpenRouter credentials are `or-*` / `openrouter-*`.
- The implementation accepts any nickname starting with `"or"`.
- Result: a name like `oracle-prod` would be treated as OpenRouter.

Relevant source:
- `src/providers/oauth.rs:55`
- `src/providers/oauth.rs:61`
