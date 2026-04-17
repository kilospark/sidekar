# Release cycle

## Normal path (CI working)

1. `./bump-version.sh patch` — syncs Cargo.toml, extension/manifest.json, www/version.txt.
2. `cargo build --release` — verify it compiles.
3. Commit the version bump along with any code changes.
4. `git tag v<version> && git push origin main v<version>` — tag + push together, same step. The release workflow only triggers on `v*` tags; pushing the commit without the tag means install.sh still downloads the old version.
5. `./pull-release.sh` — watches the GH Actions build, downloads the four-arch binaries into `www/public/binaries/<tag>/`, then runs `vercel --prod` from `www/`. This is what makes install.sh actually serve the new version.

## CI broken path (billing)

GH Actions has been failing with the account-billing error for every release from v2.0.15 onward:

> The job was not started because recent account payments have failed or your spending limit needs to be increased. Please check the 'Billing & plans' section in your settings.

When this happens, **skip the "what do you want to do?" question and just run**:

```
./local-release.sh
```

`local-release.sh` builds darwin-arm64 locally, signs with minisign, creates the GH release with that one binary, copies binaries to `www/public/binaries/<tag>/`, runs `vercel --prod`, and installs the new binary on the local machine.

Limitation: only darwin-arm64 ships. The other three targets (darwin-x64, linux-x64, linux-arm64) stay on whichever version last had a successful CI build. That's accepted — we're not waiting for billing fixes to cut releases.

## Invariants

- Always bump version before `cargo build --release`.
- Always tag and push the tag in the same step as the version-bump push.
- After tagging, either `pull-release.sh` (CI path) or `local-release.sh` (billing-down path) must run. A tag without one of those is a half-release — the binaries aren't on sidekar.dev.
