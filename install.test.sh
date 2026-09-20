#!/bin/sh
# Tests for install.sh signature verification (GitHub issue #2).
#
# install.sh must never install a binary without verifying its signature.
# These tests stub curl (so no network access is needed) and control
# whether `minisign` is on PATH, then check that install.sh refuses to
# install and exits non-zero whenever verification can't happen or fails.
#
# Both cases below exit inside the verification block, before install.sh
# ever touches the real filesystem (tar extraction, /usr/local/bin
# cleanup, shell rc files) - only HOME/INSTALL_DIR sandboxes are used,
# but that's what makes these two cases safe to run anywhere.
#
# Run: sh install.test.sh [path/to/install.sh]
set -eu

DIR="$(cd "$(dirname "$0")" && pwd)"
SCRIPT="${1:-$DIR/install.sh}"

FAIL=0
pass() { echo "ok - $1"; }
fail() { echo "not ok - $1"; FAIL=1; }

# run_case MINISIGN_MODE   ("absent" | "fail")
# Sets RC (exit code), OUT_CONTENT (combined output), INSTALLED (1 if a
# binary landed in INSTALL_DIR, else 0).
run_case() {
  mode="$1"

  WORK="$(mktemp -d)"
  FAKEBIN="$WORK/fakebin"
  INSTALL_DIR="$WORK/install"
  FAKEHOME="$WORK/home"
  mkdir -p "$FAKEBIN" "$INSTALL_DIR" "$FAKEHOME"

  # Fake curl: writes a placeholder file for whatever -o target it's
  # asked for, instead of hitting the network. Verification fails or
  # aborts before the downloaded file's content is ever used, so a
  # placeholder is enough for both cases exercised here.
  cat > "$FAKEBIN/curl" <<'CURL_EOF'
#!/bin/sh
out=""
prev=""
for a in "$@"; do
  if [ "$prev" = "-o" ]; then out="$a"; fi
  prev="$a"
done
[ -n "$out" ] || exit 0
echo "fixture" > "$out"
CURL_EOF
  chmod +x "$FAKEBIN/curl"

  if [ "$mode" = "fail" ]; then
    cat > "$FAKEBIN/minisign" <<'MS_EOF'
#!/bin/sh
exit 1
MS_EOF
    chmod +x "$FAKEBIN/minisign"
  fi
  # mode = "absent": no minisign written to $FAKEBIN, and PATH below
  # includes no other directory a system minisign could live in.

  OUT="$WORK/out.log"
  set +e
  env -i \
    HOME="$FAKEHOME" \
    PATH="$FAKEBIN:/usr/bin:/bin" \
    INSTALL_DIR="$INSTALL_DIR" \
    VERSION="v0.0.0-test" \
    sh "$SCRIPT" > "$OUT" 2>&1
  RC=$?
  set -e

  OUT_CONTENT="$(cat "$OUT")"
  if [ -e "$INSTALL_DIR/sidekar" ]; then INSTALLED=1; else INSTALLED=0; fi

  rm -rf "$WORK"
}

echo "# testing $SCRIPT"

# --- Case 1: minisign missing must fail loudly, never install ---
run_case absent
if [ "$RC" -ne 0 ]; then
  pass "missing minisign: exits non-zero (got $RC)"
else
  fail "missing minisign: expected non-zero exit, got $RC"
fi
if [ "$INSTALLED" -eq 0 ]; then
  pass "missing minisign: does not install a binary"
else
  fail "missing minisign: installed a binary despite missing minisign"
fi
if printf '%s' "$OUT_CONTENT" | grep -qi "skipping signature verification"; then
  fail "missing minisign: still silently skips verification"
else
  pass "missing minisign: does not silently skip verification"
fi
if printf '%s' "$OUT_CONTENT" | grep -qi "ERROR"; then
  pass "missing minisign: prints a clear error"
else
  fail "missing minisign: no clear error printed"
fi

# --- Case 2: minisign present but verification fails must still abort ---
run_case fail
if [ "$RC" -ne 0 ]; then
  pass "failed verification: exits non-zero (got $RC)"
else
  fail "failed verification: expected non-zero exit, got $RC"
fi
if [ "$INSTALLED" -eq 0 ]; then
  pass "failed verification: does not install a binary"
else
  fail "failed verification: installed a binary despite failed verification"
fi

exit $FAIL
