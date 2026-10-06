import { ObjectId } from "mongodb";
import { getDb } from "../../_db.js";
import { getUserOrDevice } from "../../_auth.js";
import { ensureSecretSyncIndexes } from "../../_sync-indexes.js";

// Production MONGODB_URI is only reachable from inside Vercel, so init-db.js
// can't be run against it from outside. Bootstrap the indexes here instead,
// once per function instance. A failure here must not break sync traffic;
// it just means the endpoint runs without the index until the next cold start.
const indexesReady = getDb()
  .then((db) => ensureSecretSyncIndexes(db))
  .catch((err) => {
    console.error("secret_sync index bootstrap failed:", err.message);
  });

const MAX_BATCH = 500;
const VALID_KINDS = new Set(["kv", "totp", "hotp", "memory"]);

// A page of a paged pull. A Vercel function's response body is capped at
// 4.5 MB; once memory archives synced, an account's whole history could pass
// that, and the single-response pull failed for every kind. Clients that send
// `paged=1` read their history a page at a time; older clients still get the
// single response.
const PAGE_BYTES = 3_000_000;
const PAGE_RECORDS = 1000;

function validateRecord(record) {
  if (!record || typeof record !== "object") return "record must be an object";
  const { kind, record_id, ciphertext, version } = record;
  if (!VALID_KINDS.has(kind)) return "kind must be 'kv', 'totp', 'hotp' or 'memory'";
  if (typeof record_id !== "string" || !record_id) return "record_id required";
  if (typeof ciphertext !== "string") return "ciphertext must be a string";
  if (!Number.isInteger(version) || version < 1) return "version must be a positive integer";
  if (record.device_id !== undefined && typeof record.device_id !== "string") {
    return "device_id must be a string";
  }
  if (record.deleted !== undefined && typeof record.deleted !== "boolean") {
    return "deleted must be a boolean";
  }
  return null;
}

/**
 * Atomic compare-and-swap: accept `record` only if the stored version is
 * lower than the incoming one, or no document exists yet.
 *
 * A single findOneAndUpdate with `{$or: [{version: {$lt}}, {version: {$exists:false}}]}`
 * and `upsert: true` cannot express "reject when a doc exists with an equal
 * or higher version" on its own: when that CAS filter fails to match an
 * *existing* document, Mongo's upsert path still tries to insert a new one,
 * which collides with the unique (user_id, kind, record_id) index and throws
 * E11000 instead of rejecting cleanly. Catching that error and reporting the
 * current stored version as a rejection is what makes the CAS behave the way
 * the client expects (accepted:false + current_version to re-merge from).
 */
async function upsertRecord(collection, userId, record) {
  const { kind, record_id, ciphertext, version } = record;
  const deviceId = typeof record.device_id === "string" ? record.device_id : "";
  const deleted = !!record.deleted;
  const now = new Date();

  try {
    const result = await collection.findOneAndUpdate(
      {
        user_id: userId,
        kind,
        record_id,
        $or: [{ version: { $lt: version } }, { version: { $exists: false } }],
      },
      {
        $set: { ciphertext, version, device_id: deviceId, deleted, updated_at: now },
        $setOnInsert: { user_id: userId, kind, record_id },
      },
      { upsert: true, returnDocument: "after" }
    );
    const doc = result && result.value ? result.value : result;
    return { kind, record_id, accepted: true, current_version: doc.version };
  } catch (err) {
    if (err && err.code === 11000) {
      const existing = await collection.findOne({ user_id: userId, kind, record_id });
      return {
        kind,
        record_id,
        accepted: false,
        current_version: existing ? existing.version : version,
      };
    }
    throw err;
  }
}

function toRecord(d) {
  return {
    kind: d.kind,
    record_id: d.record_id,
    ciphertext: d.ciphertext,
    version: d.version,
    device_id: d.device_id || "",
    deleted: !!d.deleted,
  };
}

/**
 * One page of the records changed after `since`, oldest first, ordered by
 * (updated_at, _id) so a page boundary never splits records that share a
 * timestamp. `afterId` resumes after the last record of the previous page.
 */
async function pagedPull(collection, userId, since, afterId) {
  // Taken before the read: a record written while this page is read is either
  // in it or newer than the watermark the client keeps.
  const serverTime = Date.now();
  const sinceDate = new Date(since);
  const filter = { user_id: userId };
  if (afterId && ObjectId.isValid(afterId)) {
    filter.$or = [
      { updated_at: { $gt: sinceDate } },
      { updated_at: sinceDate, _id: { $gt: new ObjectId(afterId) } },
    ];
  } else {
    filter.updated_at = { $gt: sinceDate };
  }

  const records = [];
  let bytes = 0;
  let last = null;
  let hasMore = false;
  for await (const d of collection.find(filter).sort({ updated_at: 1, _id: 1 })) {
    const size = (d.ciphertext || "").length;
    if (records.length > 0 && (records.length >= PAGE_RECORDS || bytes + size > PAGE_BYTES)) {
      hasMore = true;
      break;
    }
    records.push(toRecord(d));
    bytes += size;
    last = d;
  }

  return {
    records,
    server_time: serverTime,
    has_more: hasMore,
    next: hasMore ? { since: last.updated_at.getTime(), after_id: last._id.toString() } : null,
  };
}

export default async function handler(req, res) {
  const user = await getUserOrDevice(req);
  if (!user) {
    return res.status(401).json({ error: "Not authenticated" });
  }

  await indexesReady;
  const db = await getDb();
  const collection = db.collection("secret_sync");

  if (req.method === "PUT") {
    const { records } = req.body || {};
    if (!Array.isArray(records)) {
      return res.status(400).json({ error: "records must be an array" });
    }
    if (records.length > MAX_BATCH) {
      return res.status(400).json({ error: `records must not exceed ${MAX_BATCH} per batch` });
    }
    for (const record of records) {
      const error = validateRecord(record);
      if (error) {
        return res.status(400).json({ error });
      }
    }

    const results = [];
    for (const record of records) {
      results.push(await upsertRecord(collection, user.user_id, record));
    }
    return res.json({ results });
  }

  if (req.method === "GET") {
    const since = Number(req.query.since) || 0;
    if (req.query.paged) {
      return res.json(await pagedPull(collection, user.user_id, since, req.query.after_id));
    }

    const docs = await collection
      .find({ user_id: user.user_id, updated_at: { $gt: new Date(since) } })
      .sort({ updated_at: 1 })
      .toArray();

    return res.json({ records: docs.map(toRecord), server_time: Date.now() });
  }

  res.status(405).end();
}
