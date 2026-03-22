# Editor And App Agent Test Template

Use this template to evaluate editor-integrated and app-native coding-agent surfaces.

Target products:
- `VS Code`
- `Zed`
- `Cursor`
- `Codex app`
- `Claude desktop`

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
| Product | |
| Product version | |
| Agent surface | chat panel / agent panel / terminal panel / app UI |
| Agent | |
| Agent version | |
| Repo / workspace | |
| Inside integrated terminal? | yes / no |

## Capability Summary

| Capability | Result | Notes |
|---|---|---|
| Can identify target workspace | pass / fail / partial | |
| Can identify target agent session | pass / fail / partial | |
| Can focus the agent input surface | pass / fail / partial | |
| Can inject printable text | pass / fail / partial | |
| Can submit prompt | pass / fail / partial | |
| Can inject control keys | pass / fail / partial | |
| Can distinguish panel UI vs terminal UI | pass / fail / partial | |
| Can target correct tab/editor/window | pass / fail / partial | |
| Reliable across retries | pass / fail / partial | |
| Safe for background use | pass / fail / partial | |

## Environment

```text
workspace:

focused window:

focused panel/surface:

agent mode:

integrated terminal state:
```

## Discovery

| Check | Method | Expected | Actual | Result | Notes |
|---|---|---|---|---|---|
| Detect frontmost app | app API / accessibility | correct app | | | |
| Detect focused workspace | app UI / API | correct repo | | | |
| Detect target agent surface | panel / tab / view | correct surface | | | |
| Detect whether input is in terminal or custom UI | visual / AX tree | classified correctly | | | |

## Injection Tests

Run harmless markers first.

| Test ID | Scenario | Payload | Expected | Actual | Result | Notes |
|---|---|---|---|---|---|---|
| E1 | Printable text only | `EDITOR_TEXT_ONLY` | appears in input | | | |
| E2 | Text + submit | `EDITOR_SUBMIT_TEST` | submits once | | | |
| E3 | Separate submit after text | text then Enter | submits current input | | | |
| E4 | Control key clear line | `Ctrl+U` or product equivalent | line cleared | | | |
| E5 | Backspace / delete | `BS`, `DEL` | text edited | | | |
| E6 | Visible synthetic key | `Q` | `Q` appears | | | |
| E7 | Enter only | Enter | current prompt submits | | | |
| E8 | Repeatability | repeat E2 5x | same behavior each run | | | |

## Surface-Specific Tests

| Test ID | Scenario | Expected | Actual | Result | Notes |
|---|---|---|---|---|---|
| S1 | Custom chat/agent panel | input focused and editable | | | |
| S2 | Integrated terminal running agent CLI | input focused and editable | | | |
| S3 | Switching surfaces | wrong surface unaffected | | | |
| S4 | Background editor tab isolation | only target surface changes | | | |
| S5 | Busy agent state | no corruption / correct handling | | | |

## Product-Specific Notes

### VS Code
- Distinguish Copilot chat/agent UI from integrated terminal.
- Note whether external agents are running in a terminal panel or native agent panel.

### Zed
- Distinguish ACP/native agent UI from integrated terminal usage.
- Note whether the target is an ACP client, assistant panel, or terminal tab.

### Cursor
- Distinguish Cursor Agent UI from terminal panel.
- Note whether `agent` is in a custom UI or CLI in a terminal.

### Codex app
- Distinguish prompt box, task list, and background agent surfaces.
- Note whether parallel agent panes can be individually targeted.

### Claude desktop
- Distinguish chat UI from Claude Code-adjacent terminal workflows.
- Note whether the target surface is a native input box or an embedded terminal.

## Agent-Specific Tests

| Agent | Surface | Text injection | Submit works | Control keys work | Notes |
|---|---|---|---|---|---|
| `codex` | | pass / fail / partial | pass / fail / partial | pass / fail / partial | |
| `claude` | | pass / fail / partial | pass / fail / partial | pass / fail / partial | |
| `gemini` | | pass / fail / partial | pass / fail / partial | pass / fail / partial | |
| `agent` | | pass / fail / partial | pass / fail / partial | pass / fail / partial | |
| `opencode` | | pass / fail / partial | pass / fail / partial | pass / fail / partial | |

## Reliability Notes

- Did focus move unexpectedly?
- Did input go to the wrong panel or tab?
- Did the app consume Enter as UI navigation instead of submit?
- Did behavior differ between terminal-backed agents and native agent panels?
- Did the product expose enough metadata to target the right surface reliably?

## Verdict

| Category | Result | Notes |
|---|---|---|
| Feasible for text injection | yes / no / partial | |
| Feasible for submit/control | yes / no / partial | |
| Feasible for unmanaged coding agents | yes / no / partial | |
| Good replacement for `tmux send-keys` | yes / no / partial | |

## Minimal Repro

```text
Product:
Agent:
Surface:
Steps:
1.
2.
3.

Observed:

Expected:
```
