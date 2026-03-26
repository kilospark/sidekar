import { getDb } from "../_db.js";
import { getUser } from "../_auth.js";

export default async function handler(req, res) {
  const user = await getUser(req);
  if (!user) return res.status(401).json({ error: "not authenticated" });

  const { ObjectId } = await import("mongodb");
  const userId = new ObjectId(user.sub);

  if (req.method === "GET") {
    const db = await getDb();
    const docs = await db
      .collection("devices")
      .find({ user_id: userId })
      .sort({ last_seen_at: -1 })
      .toArray();

    const devices = docs.map((d) => ({
      id: d._id.toString(),
      hostname: d.hostname,
      os: d.os,
      arch: d.arch,
      sidekar_version: d.sidekar_version,
      last_seen_at: d.last_seen_at ? d.last_seen_at.toISOString() : null,
      created_at: d.created_at ? d.created_at.toISOString() : null,
    }));

    return res.json({ devices });
  }

  if (req.method === "DELETE") {
    const id = req.query.id;
    if (!id || !ObjectId.isValid(id)) {
      return res.status(400).json({ error: "valid id query parameter required" });
    }

    const db = await getDb();
    const result = await db.collection("devices").deleteOne({
      _id: new ObjectId(id),
      user_id: userId,
    });

    if (result.deletedCount === 0) {
      return res.status(404).json({ error: "device not found" });
    }
    return res.json({ ok: true });
  }

  return res.status(405).end();
}
