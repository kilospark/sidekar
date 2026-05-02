# REPL Screen Output Cleanup

## Done

1. `resolving context` only on real cache miss
2. transient status row redraw rewritten as single atomic frame
3. compaction progress and journal-written notices removed from screen
4. token footer and tool-summary transcript lines gated behind verbose mode
5. spinner output delayed until `250ms`; refresh slowed to `250ms`
6. bus messages no longer print into live REPL transcript
7. shell escape success footer removed; failures and non-zero exits still print
8. core retry/auth/model-list debug chatter moved from stderr to broker log
9. low-signal REPL status lines moved off screen (`turn complete`, relay connect, MITM attach)
10. Gemini / Vertex provider debug stderr moved to broker log

## Queue

1. Bus / relay display lane
   - add explicit inbox / notification surface for bus traffic
   - keep delivery visible without polluting main prompt/output flow

2. Startup condensation
   - collapse banner / model / credential / rate-limit lines into tighter startup surface
   - push detail to `/status`

3. Verbose/status cleanup
   - review remaining startup/status lines that still commit into transcript

4. Provider-specific debug surfaces
   - review verbose WS traces in Codex provider
   - review remaining provider stderr paths
   - normalize under one verbosity policy

## Constraints

- no loss of actual model text output
- errors remain visible
- transient status must not accumulate committed lines
- interactive prompt redraw must stay atomic
