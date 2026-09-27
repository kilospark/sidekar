// secret_sync index specs, shared between init-db.js (one-time manual run)
// and v1/sync/secrets.js (runtime self-bootstrap, since prod MONGODB_URI is
// only reachable from inside Vercel).
//
// unique on (user_id, kind, record_id): required for the sync endpoint's
// compare-and-swap correctness (see upsertRecord in v1/sync/secrets.js).
// No TTL index here: a TTL race could delete a tombstone before a late
// device pulls it.
export async function ensureSecretSyncIndexes(db) {
  const collection = db.collection("secret_sync");
  await collection.createIndex({ user_id: 1, kind: 1, record_id: 1 }, { unique: true });
  await collection.createIndex({ user_id: 1, updated_at: 1 });
}
