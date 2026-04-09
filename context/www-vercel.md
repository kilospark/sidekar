# sidekar website: Vercel deployment

The sidekar landing page and API live at `~/src/sidekar/www`, deployed on Vercel.

## Structure

```
www/
├── public/              # Static site (Vercel CDN)
│   ├── index.html       # Landing page
│   ├── dashboard.html   # Session dashboard (authed)
│   ├── sessions.html    # Active sessions (authed)
│   ├── devices.html     # Authorized devices (authed)
│   ├── approve.html     # Device auth code entry
│   ├── terms.html       # Terms of service
│   ├── privacy.html     # Privacy policy
│   ├── css/common.css   # Shared CSS (theme, nav, footer, layout)
│   ├── js/              # Page-specific JS (dashboard, sessions, devices, approve)
│   └── binaries/        # Release binaries (vX.Y.Z/ subdirs)
├── api/
│   ├── _db.js           # Shared MongoDB connection (cached via globalThis)
│   ├── _auth.js         # Auth helper (JWT token verification)
│   ├── auth/            # OAuth + device auth endpoints
│   ├── sessions.js      # GET /api/sessions: active sessions from MongoDB
│   ├── devices.js       # GET/DELETE /api/devices: device management
│   ├── v1/
│   │   └── version.js   # GET  /v1/version: GitHub release check
│   ├── download/
│   │   └── [...path].js # GET  /download/:version?/:asset: binary proxy
│   └── script.js        # GET  /install, /uninstall: shell script proxy
├── vercel.json          # Rewrites + redirects
├── package.json         # Deps: mongodb, jsonwebtoken
└── version.txt          # Current version (used by bump-version.sh)
```

## Environment variables (Vercel project settings)

- `MONGODB_URI`: MongoDB Atlas connection string
- `JWT_SECRET`: for auth token signing
- `GITHUB_CLIENT_ID` / `GITHUB_CLIENT_SECRET`: OAuth app
- `GITHUB_REPO`: optional, defaults to `kilospark/sidekar`

## Deploy

```bash
cd ~/src/sidekar/www
vercel --prod
```

## Release workflow

When GitHub Actions is available:
1. `./bump-version.sh` — bump version
2. Commit, tag `vX.Y.Z`, push with tags
3. `./pull-release.sh` — waits for CI, downloads binaries, deploys to Vercel

When GitHub Actions is unavailable (billing issues etc.):
1. `./local-release.sh` — builds, signs, packages, creates GitHub release, deploys to Vercel, installs locally

## Notes

- `_db.js` caches MongoDB connection on `globalThis` for warm function reuse
- Sessions page queries MongoDB directly (no relay proxy)
- Session liveness: relay heartbeats every 30s, sessions expire after 90s without heartbeat
- Domain: sidekar.dev (redirects from *.vercel.app)
- Minisign key: `~/src/sidekar/minisign.key` (secret), `minisign.pub` (public, in repo)
