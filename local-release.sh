#!/bin/bash
# Local release: build, sign, package, create GitHub release, deploy to Vercel, install locally.
# Usage: ./local-release.sh
# Assumes: bump-version.sh already run, changes committed and tagged.
set -e

DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DIR"

VERSION=$(cat www/version.txt 2>/dev/null | tr -d '[:space:]')
TAG="v${VERSION}"
REPO="kilospark/sidekar"
NAME="sidekar-darwin-arm64"
KEY="${SIDEKAR_MINISIGN_KEY:-$HOME/.sidekar/minisign.key}"

if [ -z "$VERSION" ]; then
  echo "Error: www/version.txt is empty or missing"
  exit 1
fi

if [ ! -f "$KEY" ]; then
  echo "Error: minisign key not found at $KEY"
  exit 1
fi

# ---- Version consistency preflight --------------------------------
# All three version strings must agree. History: several recent
# releases were shipped by hand-editing Cargo.toml + the extension
# manifest, forgetting www/version.txt, which silently stranded
# sidekar.dev at the previous version and made `sidekar update` tell
# every client "you're up to date" for three releases. This check
# fails the release before any binary is built if the trio drifts.
CARGO_VERSION=$(grep '^version = ' Cargo.toml | head -1 | sed 's/^version = "\(.*\)"/\1/')
MANIFEST_VERSION=$(grep -E '^\s*"version"' extension/manifest.json | head -1 | sed -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')
if [ "$CARGO_VERSION" != "$VERSION" ] || [ "$MANIFEST_VERSION" != "$VERSION" ]; then
  echo "Error: version mismatch across release surfaces"
  echo "  www/version.txt         = $VERSION"
  echo "  Cargo.toml              = $CARGO_VERSION"
  echo "  extension/manifest.json = $MANIFEST_VERSION"
  echo
  echo "Run ./bump-version.sh [patch|minor|major] to sync all three,"
  echo "or fix by hand — but never edit one without the others."
  exit 1
fi

echo "=== Building v${VERSION} (release) ==="
cargo build --release

echo ""
echo "=== Embedding Chrome extension ==="
rm -f assets/extension.zip
zip -r assets/extension.zip extension/
cargo build --release

echo ""
echo "=== Packaging ==="
cp target/release/sidekar "$NAME"
chmod +x "$NAME"
tar czf "${NAME}.tar.gz" "$NAME"

echo ""
echo "=== Signing ==="
echo | minisign -S -s "$KEY" -m "${NAME}.tar.gz"

echo ""
echo "=== Creating GitHub release ${TAG} ==="
gh release create "$TAG" --repo "$REPO" --generate-notes \
  "${NAME}.tar.gz" "${NAME}.tar.gz.minisig" || {
    echo "Release ${TAG} may already exist. Uploading assets..."
    gh release upload "$TAG" --repo "$REPO" --clobber \
      "${NAME}.tar.gz" "${NAME}.tar.gz.minisig"
  }

echo ""
echo "=== Copying binaries to www ==="
mkdir -p "www/public/binaries/${TAG}"
cp "${NAME}.tar.gz" "${NAME}.tar.gz.minisig" "www/public/binaries/${TAG}/"

echo ""
echo "=== Deploying to Vercel ==="
cd www
npx vercel --prod

echo ""
echo "=== Installing locally ==="
cd "$DIR"
cp target/release/sidekar ~/.local/bin/sidekar
xattr -cr ~/.local/bin/sidekar
codesign -s - ~/.local/bin/sidekar

echo ""
echo "=== Restarting daemon ==="
~/.local/bin/sidekar daemon restart >/dev/null

echo ""
echo "=== Cleaning up ==="
rm -f "$NAME" "${NAME}.tar.gz" "${NAME}.tar.gz.minisig"
# Wipe target/ — release builds bloat it to ~20GB and the artifact we
# care about (target/release/sidekar) is already installed above. Next
# build will recompile from scratch; that's the accepted trade.
cargo clean

echo ""
echo "=== Done ==="
echo "v${VERSION} released, deployed, and installed ($(sidekar -v))"
