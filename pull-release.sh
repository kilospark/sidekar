#!/bin/bash
# Wait for GitHub Actions build, download binaries, deploy to Vercel.
# Usage: ./pull-release.sh [tag]
# If no tag given, reads from www/version.txt
set -e

DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="kilospark/sidekar"

if [ -n "$1" ]; then
  TAG="$1"
else
  VERSION=$(cat "$DIR/www/version.txt" 2>/dev/null | tr -d '[:space:]')
  TAG="v${VERSION}"
fi

if [ -z "$TAG" ] || [ "$TAG" = "v" ]; then
  echo "Usage: $0 [tag]  (or ensure www/version.txt exists)"
  exit 1
fi

# ---- Version consistency preflight --------------------------------
# Same rationale as local-release.sh: refuse to deploy a tag whose
# version doesn't match the three source-of-truth files. Prevents a
# Vercel push that would serve mismatched binaries or advertise a
# version the client/extension don't know about.
EXPECTED="${TAG#v}"
WWW_VERSION=$(cat "$DIR/www/version.txt" 2>/dev/null | tr -d '[:space:]')
CARGO_VERSION=$(grep '^version = ' "$DIR/Cargo.toml" | head -1 | sed 's/^version = "\(.*\)"/\1/')
MANIFEST_VERSION=$(grep -E '^\s*"version"' "$DIR/extension/manifest.json" | head -1 | sed -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')
if [ "$WWW_VERSION" != "$EXPECTED" ] \
   || [ "$CARGO_VERSION" != "$EXPECTED" ] \
   || [ "$MANIFEST_VERSION" != "$EXPECTED" ]; then
  echo "Error: version mismatch across release surfaces for tag ${TAG}"
  echo "  tag (requested)         = $EXPECTED"
  echo "  www/version.txt         = $WWW_VERSION"
  echo "  Cargo.toml              = $CARGO_VERSION"
  echo "  extension/manifest.json = $MANIFEST_VERSION"
  echo
  echo "Run ./bump-version.sh [patch|minor|major] to sync, commit,"
  echo "tag, push, then rerun. See context/release-cycle.md."
  exit 1
fi

DEST="$DIR/www/public/binaries/${TAG}"

echo "=== Waiting for GitHub Actions build for ${TAG} ==="
while true; do
  STATUS=$(gh run list --repo "$REPO" --limit 5 --json headBranch,status,conclusion \
    --jq ".[] | select(.headBranch == \"${TAG}\") | .status" 2>/dev/null | head -1)

  if [ "$STATUS" = "completed" ]; then
    CONCLUSION=$(gh run list --repo "$REPO" --limit 5 --json headBranch,conclusion \
      --jq ".[] | select(.headBranch == \"${TAG}\") | .conclusion" 2>/dev/null | head -1)
    if [ "$CONCLUSION" = "success" ]; then
      echo "Build succeeded."
      break
    else
      echo "Build failed (${CONCLUSION}). Check GitHub Actions."
      exit 1
    fi
  elif [ -z "$STATUS" ]; then
    printf "  No run found yet, waiting...\r"
  else
    printf "  Status: %-20s\r" "$STATUS"
  fi
  sleep 15
done

echo ""
echo "=== Downloading release binaries ==="
mkdir -p "$DEST"
gh release download "$TAG" --repo "$REPO" --pattern "*.tar.gz" --pattern "*.minisig" --dir "$DEST/" --clobber
ls -lh "$DEST/"

echo ""
echo "=== Deploying to Vercel ==="
cd "$DIR/www"
vercel --prod

echo ""
echo "=== Done ==="
echo "Version ${TAG} deployed to sidekar.dev"
