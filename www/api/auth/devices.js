import { ObjectId } from "mongodb";
import { getDb } from "../_db.js";
import { getUserOrDevice } from "../_auth.js";
import { expandLinkedUserObjectIds } from "../_linkedAccounts.js";

export default async function handler(req, res) {
  const user = await getUserOrDevice(req);
  if (!user) return res.status(401).json({ error: "not authenticated" });

  const userId = new ObjectId(user.user_id);

  if (req.method === "GET") {
    const db = await getDb();
    const linkedIds = await expandLinkedUserObjectIds(db, userId, "devices");

    const userRows = await db
      .collection("users")
      .find({ _id: { $in: linkedIds } })
      .project({ login: 1, name: 1 })
      .toArray();
    const meta = Object.fromEntries(
      userRows.map((u) => [u._id.toString(), { login: u.login || "", name: u.name || "" }])
    );

    const docs = await db
      .collection("devices")
      .find({ user_id: { $in: linkedIds } })
      .sort({ last_seen_at: -1 })
      .toArray();

    const selfStr = userId.toString();

    const devices = docs.map((d) => {
      const oid = d.user_id.toString();
      const isOwn = oid === selfStr;
      const m = meta[oid] || {};
      return {
        id: d._id.toString(),
        hostname: d.hostname,
        os: d.os,
        arch: d.arch,
        sidekar_version: d.sidekar_version,
        last_seen_at: d.last_seen_at ? d.last_seen_at.toISOString() : null,
        created_at: d.created_at ? d.created_at.toISOString() : null,
        from_linked_account: !isOwn,
        owner_login: !isOwn ? m.login || null : null,
        owner_name: !isOwn ? m.name || null : null,
      };
    });

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
