# REPL Screen Output Cleanup

## Done

1. `resolving context` only on real cache miss
2. transient status row redraw rewritten as single atomic frame
3. compaction progress and journal-written notices removed from screen
4. token footer and tool-summary transcript lines gated behind verbose mode

## Queue

1. Spinner policy
   - keep static status for short waits
   - start spinner only after latency threshold
   - slow tick rate from current `80ms`

2. Bus / relay display lane
   - stop injecting bus messages into main transcript stream
   - move to notification lane or explicit inbox view

3. Shell escape footer policy
   - hide `[elapsed]` on success
   - keep non-zero exit and failures visible

4. Retry/auth diagnostics routing
   - route retry notices and auth-refresh chatter through broker log or transient status
   - avoid raw `eprintln!` bypassing REPL renderer

5. Startup condensation
   - collapse banner / model / credential / rate-limit lines into tighter startup surface
   - push detail to `/status`

6. Verbose/status cleanup
   - review `[turn complete]`
   - review MITM attach line
   - review other dim status lines that still commit into transcript

7. Provider-specific debug surfaces
   - review verbose WS traces in Codex provider
   - review model-list debug output paths
   - normalize under one verbosity policy

## Constraints

- no loss of actual model text output
- errors remain visible
- transient status must not accumulate committed lines
- interactive prompt redraw must stay atomic
