import { randomBytes, createHash } from "crypto";
import { ObjectId } from "mongodb";
import { getDb } from "../_db.js";
import { getUser } from "../_auth.js";

export default async function handler(req, res) {
  if (req.method !== "POST") return res.status(405).end();

  const action = req.query.action || "create";

  if (action === "create") return handleCreate(req, res);
  if (action === "token") return handleToken(req, res);
  if (action === "approve") return handleApprove(req, res);
  if (action === "metadata") return handleMetadata(req, res);
  if (action === "ext-generate") return generateExtToken(req, res);
  if (action === "ext-verify") return verifyExtToken(req, res);

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

/** CLI reports hostname / OS / arch / version after login (Bearer = device token). */
async function handleMetadata(req, res) {
  const authHeader = req.headers.authorization || "";
  const m = authHeader.match(/^Bearer\s+(.+)$/i);
  if (!m) return res.status(401).json({ error: "Authorization: Bearer <device_token> required" });

  const token = m[1].trim();
  const tokenHash = createHash("sha256").update(token).digest("hex");

  const { hostname, os, arch, sidekar_version } = req.body || {};

  const db = await getDb();
  const result = await db.collection("devices").updateOne(
    { token_hash: tokenHash },
    {
      $set: {
        hostname: typeof hostname === "string" && hostname.trim() ? hostname.trim() : "unknown",
        os: typeof os === "string" && os.trim() ? os.trim() : "unknown",
        arch: typeof arch === "string" && arch.trim() ? arch.trim() : "unknown",
        sidekar_version:
          typeof sidekar_version === "string" && sidekar_version.trim()
            ? sidekar_version.trim()
            : "unknown",
        last_seen_at: new Date(),
      },
    }
  );

  if (result.matchedCount === 0) {
    return res.status(404).json({ error: "unknown device token" });
  }
  res.json({ ok: true });
}

async function generateExtToken(req, res) {
  const user = await getUser(req);
  if (!user) return res.status(401).json({ error: "not authenticated" });

  const db = await getDb();
  const token = randomBytes(32).toString("hex");
  const tokenHash = createHash("sha256").update(token).digest("hex");

  // Each sign-in creates a new token (multiple browsers can coexist).
  // Clean up old tokens for this user (keep at most 10).
  const existing = await db.collection("ext_tokens")
    .find({ user_id: user.sub })
    .sort({ created_at: -1 })
    .skip(9)
    .toArray();
  if (existing.length > 0) {
    await db.collection("ext_tokens").deleteMany({
      _id: { $in: existing.map((d) => d._id) },
    });
  }

  await db.collection("ext_tokens").insertOne({
    user_id: user.sub,
    token_hash: tokenHash,
    created_at: new Date(),
  });

  // Fetch user profile for extension display
  const userDoc = await db.collection("users").findOne({ _id: new ObjectId(user.sub) });
  const profile = {};
  if (userDoc) {
    if (userDoc.login) profile.login = userDoc.login;
    if (userDoc.email) profile.email = userDoc.email;
    if (userDoc.github_id) profile.provider = "github";
    else if (userDoc.google_id) profile.provider = "google";
  }

  res.json({ token, profile });
}

async function verifyExtToken(req, res) {
  const auth = req.headers.authorization;
  if (!auth || !auth.startsWith("Bearer ")) {
    return res.status(401).json({ error: "device token required" });
  }
  const deviceToken = auth.slice(7).trim();
  const deviceHash = createHash("sha256").update(deviceToken).digest("hex");

  const { ext_token } = req.body;
  if (!ext_token) return res.status(400).json({ error: "ext_token required" });
  const extHash = createHash("sha256").update(ext_token).digest("hex");

  const db = await getDb();

  const device = await db.collection("devices").findOne({ token_hash: deviceHash });
  if (!device) return res.status(401).json({ error: "invalid device token" });

  const ext = await db.collection("ext_tokens").findOne({ token_hash: extHash });
  if (!ext) return res.status(401).json({ error: "invalid ext token" });

  const isMatch = device.user_id.toString() === ext.user_id.toString();
  res.json({ match: isMatch, user_id: isMatch ? device.user_id.toString() : null });
}
