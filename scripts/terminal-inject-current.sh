#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: terminal-inject-current.sh [--dry-run] [--tty /dev/ttysNNN] <text>

Inject text into a Terminal.app tab identified by tty.

Behavior:
- Outside tmux: targets the current tty from `tty`.
- Inside tmux: targets the enclosing Terminal.app client tty from
  `tmux display-message -p '#{client_tty}'`.

Important:
- When running inside tmux, Terminal.app can only inject into the outer tab.
  The text reaches this process only if the current pane is the active pane
  in that Terminal window/tab.
- `do script` submits the text as a command. This is suitable for shell/tmux
  use, not for arbitrary full-screen TUIs.
EOF
}

dry_run=0
target_tty=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)
      dry_run=1
      shift
      ;;
    --tty)
      if [[ $# -lt 2 ]]; then
        echo "--tty requires a value" >&2
        exit 1
      fi
      target_tty="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      break
      ;;
    -*)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
    *)
      break
      ;;
  esac
done

if [[ $# -lt 1 ]]; then
  usage >&2
  exit 1
fi

payload="$*"
mode="direct"
current_tty="$(tty)"

if [[ -z "$target_tty" ]]; then
  target_tty="$current_tty"
  if [[ -n "${TMUX:-}" ]]; then
    if ! command -v tmux >/dev/null 2>&1; then
      echo "tmux is not available, but TMUX is set" >&2
      exit 1
    fi
    pane_target="${TMUX_PANE:-}"
    if [[ -z "$pane_target" ]]; then
      echo "TMUX is set but TMUX_PANE is empty" >&2
      exit 1
    fi
    target_tty="$(tmux display-message -p -t "$pane_target" '#{client_tty}')"
    pane_tty="$(tmux display-message -p -t "$pane_target" '#{pane_tty}')"
    mode="tmux"
  fi
fi

if [[ "$target_tty" != /dev/* ]]; then
  echo "Refusing to target non-device tty: $target_tty" >&2
  exit 1
fi

{
  echo "mode=$mode"
  echo "current_tty=$current_tty"
  if [[ "${pane_tty:-}" != "" ]]; then
    echo "pane_tty=$pane_tty"
  fi
  echo "target_tty=$target_tty"
  echo "payload=$payload"
} >&2

if [[ "$mode" == "tmux" ]]; then
  echo "tmux note: Terminal.app can only inject into the enclosing tab." >&2
  echo "tmux note: the target pane must be the active pane in that tab." >&2
fi

if [[ $dry_run -eq 1 ]]; then
  exit 0
fi

osascript - "$target_tty" "$payload" <<'APPLESCRIPT'
on run argv
  set targetTty to item 1 of argv
  set payload to item 2 of argv

  tell application "Terminal"
    repeat with w in windows
      repeat with t in tabs of w
        if (tty of t) is equal to targetTty then
          do script payload in t
          return "Injected into " & targetTty
        end if
      end repeat
    end repeat
  end tell

  error "No Terminal.app tab found for tty " & targetTty
end run
APPLESCRIPT
