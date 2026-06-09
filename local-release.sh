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
GH_TOKEN="$(gh auth token)"

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
mkdir -p assets
# Zip contents of extension/, not the folder itself — must match
# .github/workflows/release.yml or dev-extract lands at
# ~/.sidekar/extension/extension/.
(cd extension && zip -r ../assets/extension.zip . -x '*.test.*' 'generate_icons.py' 'README.md')
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
echo "=== Publishing GitHub release ${TAG} ==="
# `gh release create/upload` has been observed to hang indefinitely
# after creating draft release or uploading first asset. Create draft
# with `gh`, then upload assets via GitHub uploads API so reruns can
# safely recover and clobber existing assets.
if gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
  echo "Release ${TAG} already exists. Reusing it."
else
  gh release create "$TAG" --repo "$REPO" --draft --title "$TAG" --notes ""
fi
RELEASE_ID="$(
  gh api "repos/${REPO}/releases/tags/${TAG}" --jq '.id' 2>/dev/null \
    || gh api "repos/${REPO}/releases" --jq ".[] | select(.tag_name==\"${TAG}\") | .id" | head -n 1
)"
if [ -z "$RELEASE_ID" ] || [ "$RELEASE_ID" = "null" ]; then
  echo "Error: could not resolve GitHub release id for ${TAG}"
  exit 1
fi
upload_asset() {
  local file="$1"
  local asset_name existing_id content_type
  asset_name="$(basename "$file")"
  existing_id="$(
    gh api "repos/${REPO}/releases/tags/${TAG}" \
      --jq ".assets[] | select(.name == \"${asset_name}\") | .id" 2>/dev/null \
      | head -n 1 || true
  )"
  if [ -n "$existing_id" ]; then
    gh api -X DELETE "repos/${REPO}/releases/assets/${existing_id}" >/dev/null
  fi
  content_type="application/octet-stream"
  case "$asset_name" in
    *.tar.gz) content_type="application/gzip" ;;
  esac
  curl -fsSL -X POST \
    --http1.1 \
    -H "Authorization: Bearer ${GH_TOKEN}" \
    -H "Content-Type: ${content_type}" \
    -H "Expect:" \
    --limit-rate 100k \
    --data-binary "@${file}" \
    "https://uploads.github.com/repos/${REPO}/releases/${RELEASE_ID}/assets?name=${asset_name}" >/dev/null
}
upload_asset "${NAME}.tar.gz"
upload_asset "${NAME}.tar.gz.minisig"
gh release edit "$TAG" --repo "$REPO" --draft=false --title "$TAG"

echo ""
echo "=== Staging binaries for Vercel ==="
rm -rf "www/public/binaries"
DEST="www/public/binaries/${TAG}"
mkdir -p "$DEST"
cp "${NAME}.tar.gz" "${NAME}.tar.gz.minisig" "$DEST/"
ls -lh "$DEST/"

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
