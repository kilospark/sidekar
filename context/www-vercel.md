# sidekar website — Vercel deployment

The sidekar landing page and API have been migrated from a Docker/Cloud Run Express app (`vm-sites/sidekar-api`) to Vercel serverless functions at `~/src/sidekar/www`.

## Previous setup

- Repo: `vm-sites/sidekar-api` (git submodule)
- Runtime: Express 5 on Node.js 22 (Alpine Docker image)
- Deployed to Cloud Run, serving both static files and API routes
- Domain: sidekar.space

## Current setup

- Location: `~/src/sidekar/www/` (inside the main sidekar repo)
- Platform: Vercel (serverless functions + static CDN)
- No Express — each route is a standalone serverless function
- Static files served from `public/` by Vercel's CDN

## Structure

```
www/
├── public/              # Landing page, favicons, manifest (Vercel CDN)
├── api/
│   ├── _db.js           # Shared MongoDB connection (cached via globalThis)
│   ├── v1/
│   │   ├── version.js   # GET  /v1/version — GitHub release check
│   │   ├── telemetry.js # POST /v1/telemetry — session telemetry
│   │   ├── feedback.js  # POST /v1/feedback — user ratings
│   │   └── stats.js     # GET  /v1/stats — aggregated tool/rating stats
│   ├── download/
│   │   └── [...path].js # GET  /download/:version?/:asset — binary proxy
│   └── script.js        # GET  /install, /uninstall — shell script proxy
├── vercel.json          # Rewrites + host redirect
├── package.json         # Only dep: mongodb
├── init-db.js           # One-time index creation script
└── .gitignore
```

## URL mapping

Clients hit the same URLs as before. `vercel.json` rewrites map them to serverless functions:

| Public URL | Function |
|---|---|
| `/v1/version` | `api/v1/version.js` |
| `/v1/telemetry` | `api/v1/telemetry.js` |
| `/v1/feedback` | `api/v1/feedback.js` |
| `/v1/stats` | `api/v1/stats.js` |
| `/download/*` | `api/download/[...path].js` |
| `/install` | `api/script.js?name=install` |
| `/uninstall` | `api/script.js?name=uninstall` |

## Environment variables

Set in Vercel project settings:

- `MONGODB_URI` — MongoDB Atlas connection string (same `sidekar` DB as before)
- `GITHUB_REPO` — optional, defaults to `kilospark/sidekar`

## Host redirect

Non-canonical hosts (e.g. `*.vercel.app`, `www`) are 301-redirected to `sidekar.space` via `vercel.json` redirects (replaces the Express middleware).

## Deploy

```bash
cd ~/src/sidekar/www
npm install
vercel          # preview
vercel --prod   # production
```

## DB setup

Same MongoDB database (`sidekar`) with collections `telemetry` and `feedback`. Run `init-db.js` once to create indexes:

```bash
MONGODB_URI="mongodb+srv://..." node init-db.js
```

## Notes

- The `_db.js` module caches the MongoDB connection on `globalThis` so it persists across warm function invocations
- Version and stats endpoints have 5-minute in-memory caches (same as the Express version)
- The download proxy streams GitHub release binaries through the function
