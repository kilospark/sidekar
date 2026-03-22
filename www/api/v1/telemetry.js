import { getDb } from "../_db.js";

export default async function handler(req, res) {
  if (req.method !== "POST") return res.status(405).end();

  const { session_id, version, platform, duration_s, tools } = req.body;
  if (!session_id) return res.status(400).json({ error: "session_id required" });

  const db = await getDb();
  await db.collection("telemetry").insertOne({
    session_id,
    version: version || "unknown",
    platform: platform || "unknown",
    duration_s: duration_s || 0,
    tools: tools || {},
    created_at: new Date(),
  });

  res.json({ ok: true });
}
