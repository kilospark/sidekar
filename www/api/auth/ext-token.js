import { createHash, randomBytes } from "crypto";
import { getDb } from "../_db.js";
import { getUser } from "../_auth.js";

export default async function handler(req, res) {
  if (req.method !== "POST") return res.status(405).end();

  // ?verify=1 is the verify-ext endpoint (called by CLI ext-server)
  if (req.query.verify) {
    return verifyExtToken(req, res);
  }

  // Default: generate new ext token (called by ext-callback page)
  return generateExtToken(req, res);
}

async function generateExtToken(req, res) {
  const user = await getUser(req);
  if (!user) return res.status(401).json({ error: "not authenticated" });

  const db = await getDb();
  const token = randomBytes(32).toString("hex");
  const tokenHash = createHash("sha256").update(token).digest("hex");

  await db.collection("ext_tokens").updateOne(
    { user_id: user.sub },
    {
      $set: {
        token_hash: tokenHash,
        updated_at: new Date(),
      },
      $setOnInsert: { created_at: new Date() },
    },
    { upsert: true }
  );

  res.json({ token });
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
