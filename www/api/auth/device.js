import { randomBytes, createHash } from "crypto";
import { getDb } from "../_db.js";
import { getUser } from "../_auth.js";

export default async function handler(req, res) {
  if (req.method !== "POST") return res.status(405).end();

  const action = req.query.action || "create";

  if (action === "create") return handleCreate(req, res);
  if (action === "token") return handleToken(req, res);
  if (action === "approve") return handleApprove(req, res);

  return res.status(400).json({ error: "unknown action" });
}

async function handleCreate(_req, res) {
  const deviceCode = randomBytes(16).toString("hex");
  const chars = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
  let userCode = "";
  for (let i = 0; i < 8; i++) {
    if (i === 4) userCode += "-";
    userCode += chars[randomBytes(1)[0] % chars.length];
  }

  const db = await getDb();
  await db.collection("device_codes").insertOne({
    device_code: deviceCode,
    user_code: userCode,
    user_id: null,
    expires_at: new Date(Date.now() + 15 * 60 * 1000),
    created_at: new Date(),
  });

  res.json({
    device_code: deviceCode,
    user_code: userCode,
    verification_uri: "https://sidekar.dev/approve",
    expires_in: 900,
    interval: 5,
  });
}

async function handleToken(req, res) {
  const { device_code } = req.body;
  if (!device_code) return res.status(400).json({ error: "device_code required" });

  const db = await getDb();
  const doc = await db.collection("device_codes").findOne({ device_code });

  if (!doc) return res.status(404).json({ error: "expired" });
  if (!doc.user_id) return res.json({ status: "pending" });

  const token = doc.token;
  await db.collection("device_codes").deleteOne({ _id: doc._id });
  res.json({ status: "approved", token });
}

async function handleApprove(req, res) {
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

  const token = randomBytes(32).toString("hex");
  const tokenHash = createHash("sha256").update(token).digest("hex");

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

  await db.collection("device_codes").updateOne(
    { _id: doc._id },
    { $set: { user_id: user.sub, token } }
  );

  res.json({ ok: true });
}
