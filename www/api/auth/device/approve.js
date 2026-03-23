import { getDb } from "../../_db.js";
import { getUser } from "../../_auth.js";
import { randomBytes, createHash } from "crypto";

export default async function handler(req, res) {
  if (req.method !== "POST") return res.status(405).end();

  const user = await getUser(req);
  if (!user) return res.status(401).json({ error: "not authenticated" });

  const { user_code, hostname, os, arch, sidekar_version } = req.body;
  if (!user_code) return res.status(400).json({ error: "user_code required" });

  const db = await getDb();
  const doc = await db.collection("device_codes").findOne({
    user_code: user_code.toUpperCase().trim(),
    user_id: null,
  });

  if (!doc) return res.status(404).json({ error: "invalid or expired code" });

  // Generate device token
  const token = randomBytes(32).toString("hex");
  const tokenHash = createHash("sha256").update(token).digest("hex");

  // Create device record
  const { ObjectId } = await import("mongodb");
  await db.collection("devices").insertOne({
    user_id: new ObjectId(user.sub),
    token_hash: tokenHash,
    hostname: hostname || "unknown",
    os: os || "unknown",
    arch: arch || "unknown",
    sidekar_version: sidekar_version || "unknown",
    last_seen_at: new Date(),
    created_at: new Date(),
  });

  // Mark device code as approved (store token for polling endpoint)
  await db.collection("device_codes").updateOne(
    { _id: doc._id },
    { $set: { user_id: user.sub, token } }
  );

  res.json({ ok: true });
}
