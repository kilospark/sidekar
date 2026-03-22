# Terminal Agent Test Template

Use this template to evaluate terminals and terminal-adjacent shells for coding-agent control.

Target products:
- `Terminal.app`
- `iTerm2`
- `Warp`
- `WezTerm`
- `kitty`
- `Ghostty`

Target agents:
- `codex`
- `claude`
- `gemini`
- `agent` (Cursor)
- `opencode`
- `copilot`
- `aider`

## Test Metadata

| Field | Value |
|---|---|
| Date | |
| Tester | |
| Machine | |
| OS version | |
| Terminal product | |
| Terminal version | |
| Agent | |
| Agent version | |
| Inside `tmux`? | yes / no |
| Shell | |
| Focused surface | terminal / editor panel / app UI |

## Capability Summary

| Capability | Result | Notes |
|---|---|---|
| Can identify target session | pass / fail / partial | |
| Can target by `tty` / pane / session id | pass / fail / partial | |
| Can inject printable text | pass / fail / partial | |
| Can inject submit/Enter | pass / fail / partial | |
| Can inject control keys | pass / fail / partial | |
| Works while inside `tmux` | pass / fail / partial | |
| Works on coding-agent prompt | pass / fail / partial | |
| Reliable across retries | pass / fail / partial | |
| No focus stealing required | pass / fail / partial | |
| Safe for background use | pass / fail / partial | |

## Environment

```text
pwd:

tty:

TMUX:

TMUX_PANE:

agent process tree:
```

## Discovery

| Check | Command / Method | Expected | Actual | Result | Notes |
|---|---|---|---|---|---|
| Detect current `tty` | `tty` | current tty visible | | | |
| Detect enclosing terminal session | app-specific | session found | | | |
| Detect focused agent surface | visual / app API | correct target | | | |
| Detect target identity | tty / pane / session id | stable identifier | | | |

## Injection Tests

Run these in order. Use harmless markers, not shell commands, unless explicitly testing command execution.

| Test ID | Scenario | Payload | Expected | Actual | Result | Notes |
|---|---|---|---|---|---|---|
| T1 | Printable text only | `TERMINAL_TEXT_ONLY` | appears in input | | | |
| T2 | Text + submit | `TERMINAL_SUBMIT_TEST` | submits once | | | |
| T3 | Separate submit after text | text then Enter | submits current input | | | |
| T4 | Control key clear line | `Ctrl+U` | current line cleared | | | |
| T5 | Backspace / delete | `BS`, `DEL` | text edited | | | |
| T6 | Visible synthetic key | `Q` | `Q` appears | | | |
| T7 | Enter only | Enter | current prompt submits | | | |
| T8 | Repeatability | repeat T2 5x | same behavior each run | | | |

## `tmux`-Specific Tests

Only fill this section when testing inside `tmux`.

| Test ID | Scenario | Expected | Actual | Result | Notes |
|---|---|---|---|---|---|
| M1 | Target enclosing terminal tab | correct tab selected | | | |
| M2 | Active pane receives injection | current pane changes | | | |
| M3 | Inactive pane isolation | inactive pane unaffected | | | |
| M4 | Works in simple stdin reader under `tmux` | line received | | | |
| M5 | Works in real coding-agent prompt under `tmux` | agent prompt responds | | | |

## Agent-Specific Tests

Repeat for each agent you care about.

| Agent | Prompt state | Text injection | Submit works | Control keys work | Notes |
|---|---|---|---|---|---|
| `codex` | idle / busy / waiting | pass / fail / partial | pass / fail / partial | pass / fail / partial | |
| `claude` | idle / busy / waiting | pass / fail / partial | pass / fail / partial | pass / fail / partial | |
| `gemini` | idle / busy / waiting | pass / fail / partial | pass / fail / partial | pass / fail / partial | |
| `agent` | idle / busy / waiting | pass / fail / partial | pass / fail / partial | pass / fail / partial | |
| `opencode` | idle / busy / waiting | pass / fail / partial | pass / fail / partial | pass / fail / partial | |

## Reliability Notes

- Was the target app frontmost?
- Did focus move unexpectedly?
- Did text arrive as paste or as typed input?
- Did submit behave differently for shell vs coding-agent UI?
- Did behavior differ between idle and busy agent states?
- Were retries needed?
- Did any action affect the wrong window, tab, or pane?

## Verdict

| Category | Result | Notes |
|---|---|---|
| Feasible for text injection | yes / no / partial | |
| Feasible for submit/control | yes / no / partial | |
| Feasible for unmanaged coding agents | yes / no / partial | |
| Good replacement for `tmux send-keys` | yes / no / partial | |

## Minimal Repro

```text
Terminal:
Agent:
Inside tmux:
Steps:
1.
2.
3.

Observed:

Expected:
```
