#!/usr/bin/env bash
# Install Sidekar from this repo into Cargo's bin dir, then expose it on PATH via ~/.local/bin
# (many shells prefer ~/.local/bin ahead of ~/.cargo/bin — symlink keeps them identical).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cargo install --path "$ROOT" --locked --force
mkdir -p "${HOME}/.local/bin"
ln -sf "${HOME}/.cargo/bin/sidekar" "${HOME}/.local/bin/sidekar"

# macOS: freshly installed binaries can carry xattrs that trigger Gatekeeper SIGKILL (exit 137).
# See context/feedback_macos_binary_xattr.md
if [[ "$(uname -s)" == "Darwin" ]]; then
    xattr -cr "${HOME}/.cargo/bin/sidekar"
    codesign --force --sign - "${HOME}/.cargo/bin/sidekar"
fi

echo "Active binary (first on PATH): $(command -v sidekar)"
"${HOME}/.local/bin/sidekar" --version
