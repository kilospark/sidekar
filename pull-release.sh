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
