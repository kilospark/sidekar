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
# Stage only the release being deployed. Each version is ~28MB across four
# targets, and Vercel bundles public/binaries into the api/download function,
# which is capped at 250MB uncompressed. Keeping every past release silently
# grew that function until a deploy failed with the GitHub release already
# published — the worst moment to find out. Older versions still download:
# api/download falls back to the GitHub release when a file is not staged.
rm -rf "$DIR/www/public/binaries"
mkdir -p "$DEST"
gh release download "$TAG" --repo "$REPO" --pattern "*.tar.gz" --pattern "*.minisig" --dir "$DEST/" --clobber
ls -lh "$DEST/"

echo ""
echo "=== Deploying to Vercel ==="
cd "$DIR/www"

# Node ships its own CA bundle and ignores the macOS keychain, so it rejects
# roots the rest of the system already trusts. On 2026-09-22 that was ISRG
# "Root YR" (issued May 2026, cross-signed by ISRG Root X1): curl and gh were
# fine, `npx vercel` failed with "unable to get local issuer certificate", and
# the release stopped with the tag already pushed and the binaries published.
#
# Rather than pin that one root, rebuild a bundle from whatever vercel.com is
# actually serving plus the system store. Self-healing: the next root rotation
# needs no edit here. Nothing is trusted that the system does not already
# trust, because the chain is verified against the system store first.
CA_BUNDLE=""
if ! node -e 'require("https").get("https://vercel.com/.well-known/openid-configuration",r=>process.exit(0)).on("error",()=>process.exit(1))' 2>/dev/null; then
  echo "  Node rejects vercel.com's chain; rebuilding a CA bundle it accepts."
  CHAIN="$(mktemp)"; CA_BUNDLE="$(mktemp)"
  openssl s_client -connect vercel.com:443 -servername vercel.com -showcerts </dev/null 2>/dev/null     | sed -n '/BEGIN CERT/,/END CERT/p' > "$CHAIN"
  if openssl verify -CAfile /etc/ssl/cert.pem "$CHAIN" >/dev/null 2>&1; then
    cat "$CHAIN" /etc/ssl/cert.pem > "$CA_BUNDLE"
    export NODE_EXTRA_CA_CERTS="$CA_BUNDLE"
    echo "  Chain verifies against the system store; trusting it for this deploy."
  else
    echo "  Chain does NOT verify against the system store — not trusting it." >&2
    echo "  Deploy by hand after checking what is terminating TLS." >&2
    exit 1
  fi
  rm -f "$CHAIN"
fi

npx vercel --prod
[ -n "$CA_BUNDLE" ] && rm -f "$CA_BUNDLE"

echo ""
echo "=== Done ==="
echo "Version ${TAG} deployed to sidekar.dev"
