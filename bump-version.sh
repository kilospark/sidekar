#!/bin/bash
# Usage: ./bump-version.sh [major|minor|patch]
# Default: patch

set -e

TYPE=${1:-patch}
DIR="$(cd "$(dirname "$0")" && pwd)"

# Read current version from Cargo.toml (source of truth)
CURRENT=$(grep '^version = ' "$DIR/Cargo.toml" | head -1 | sed 's/^version = "\(.*\)"/\1/')

IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT"

case "$TYPE" in
  major) MAJOR=$((MAJOR + 1)); MINOR=0; PATCH=0 ;;
  minor) MINOR=$((MINOR + 1)); PATCH=0 ;;
  patch) PATCH=$((PATCH + 1)) ;;
  *) echo "Usage: $0 [major|minor|patch]"; exit 1 ;;
esac

NEW="${MAJOR}.${MINOR}.${PATCH}"

# Update all version files (JSON manifests)
sed -i '' "s/\"version\": *\"$CURRENT\"/\"version\": \"$NEW\"/g" \
  "$DIR/.claude-plugin/plugin.json" \
  "$DIR/.claude-plugin/marketplace.json" \
  "$DIR/extension/manifest.json"

# Update Cargo.toml version
sed -i '' "s/^version = \".*\"/version = \"$NEW\"/" "$DIR/Cargo.toml"

# Update www/version.txt for Vercel deployment
echo "$NEW" > "$DIR/www/version.txt"

echo "$CURRENT -> $NEW"
