# Feedback DB Runbook

Use this note for future Sidekar feedback triage and status updates. Do not rediscover the DB path each time.

## Source of Truth

- Website + API: `~/src/sidekar/www` (Vercel serverless functions)
- Previous location: `vm-sites/sidekar-api` (deprecated Express app)
- DB name: `sidekar`
- Collection: `feedback`

`MONGODB_URI` is set in Vercel project env vars. For local access, pull with `vercel env pull` or set manually.

## Load Mongo Connection

```bash
set -a
. /Users/karthik/src/vm-sites/env/sidekar-api.env.sh
set +a
```

## Inspect Latest Open Feedback

```bash
mongosh "$MONGODB_URI" --quiet --eval '
const dbx = db.getSiblingDB("sidekar");
printjson(
  dbx.feedback
    .find({ status: { $in: [null, "deferred", "split"] } })
    .sort({ created_at: -1 })
    .limit(10)
    .toArray()
);
'
```

## Update Status By ID

```bash
mongosh "$MONGODB_URI" --quiet --eval '
const dbx = db.getSiblingDB("sidekar");
const id = ObjectId("PUT_OBJECT_ID_HERE");
printjson(dbx.feedback.findOne({ _id: id }, { status: 1, comment: 1 }));
printjson(dbx.feedback.updateOne(
  { _id: id },
  { $set: { status: "implemented" } }
));
printjson(dbx.feedback.findOne({ _id: id }, { status: 1, comment: 1 }));
'
```

## Split Multi-Item Feedback

For mixed feedback:

- leave the original item as `status: "split"`
- create child feedback items for each concrete request
- mark implemented children `implemented`
- leave unresolved children `null` or `deferred`

## Status Meanings

- `null`: open / not triaged
- `implemented`: shipped
- `deferred`: valid but not prioritized now
- `discarded`: rejected
- `split`: parent item was broken into child items

## Rules

- Do not discard anything without explicit user sign-off.
- For follow-up status changes, use the known API env path above instead of searching the filesystem again.
