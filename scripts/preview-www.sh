#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${1:-8080}"
exec python3 "$ROOT/www/preview.py" "$PORT"
