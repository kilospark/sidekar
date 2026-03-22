import { getDb } from "../_db.js";

export default async function handler(req, res) {
  if (req.method !== "POST") return res.status(405).end();

  const { session_id, version, rating, comment } = req.body;
  if (!session_id) return res.status(400).json({ error: "session_id required" });
  if (!rating || typeof rating !== "number" || rating < 1 || rating > 5) {
    return res.status(400).json({ error: "rating required (integer 1-5)" });
  }

  const db = await getDb();
  await db.collection("feedback").insertOne({
    session_id,
    version: version || "unknown",
    rating,
    comment: comment || "",
    status: null,
    created_at: new Date(),
  });

  res.json({ ok: true });
}
