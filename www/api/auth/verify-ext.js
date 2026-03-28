import { createHash } from "crypto";
import { getDb } from "../_db.js";

export default async function handler(req, res) {
  if (req.method !== "POST") return res.status(405).end();

  // CLI device token in Authorization header
  const auth = req.headers.authorization;
  if (!auth || !auth.startsWith("Bearer ")) {
    return res.status(401).json({ error: "device token required" });
  }
  const deviceToken = auth.slice(7).trim();
  const deviceHash = createHash("sha256").update(deviceToken).digest("hex");

  // Extension token in body
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
