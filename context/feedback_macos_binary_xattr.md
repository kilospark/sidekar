---
name: macOS local binary xattr fix
description: Local cargo builds need xattr/codesign after cp to PATH to avoid Gatekeeper SIGKILL
type: feedback
---

After `cp target/release/sidekar ~/.local/bin/sidekar`, macOS adds `com.apple.provenance` xattr causing Gatekeeper to SIGKILL (exit 137). Fix:

```
xattr -cr ~/.local/bin/sidekar
codesign -fs - ~/.local/bin/sidekar
```

**Why:** macOS quarantine flags locally-built binaries copied via `cp`. Not needed for GitHub Release downloads (tar extraction doesn't add the xattr) or install.sh curl pipeline.

**How to apply:** When testing local builds, always run the two commands after cp. Only affects local dev workflow, not release distribution.
