import { getUserOrDevice } from "./_auth.js";
import { getDb } from "./_db.js";

const SESSION_TTL_MS = 90 * 1000; // matches relay's SESSION_TTL_SECS

let discoverIndexEnsured = false;

export default async function handler(req, res) {
  // Route: ?discover — daemon port discovery
  if ("discover" in (req.query || {})) {
    return handleDiscover(req, res);
  }

  if (req.method !== "GET") return res.status(405).end();

  const user = await getUserOrDevice(req);
  if (!user) return res.status(401).json({ error: "not authenticated" });

  try {
    const userId = user.user_id;
    if (!userId) {
      return res.status(401).json({ error: "invalid session token" });
    }

    const db = await getDb();
    const cutoff = new Date(Date.now() - SESSION_TTL_MS);

    const docs = await db
      .collection("sessions")
      .find({
        user_id: userId,
        last_heartbeat: { $gt: cutoff },
      })
      .sort({ connected_at: -1 })
      .toArray();

    const sessions = docs.map((d) => ({
      id: d.session_id,
      name: d.name || "",
      agent_type: d.agent_type || "",
      cwd: d.cwd || "",
      hostname: d.hostname || "",
      nickname: d.nickname || null,
      connected_at: d.connected_at,
      relay_url: d.owner_origin || null,
    }));

    res.json({ sessions });
  } catch (err) {
    console.error("sessions list failed:", err);
    return res.status(500).json({ error: "failed to load sessions" });
  }
}

async function handleDiscover(req, res) {
  const user = await getUserOrDevice(req);
  if (!user) return res.status(401).json({ error: "not authenticated" });

  const userId = user.user_id;
  if (!userId) return res.status(401).json({ error: "invalid session" });

  const db = await getDb();
  const col = db.collection("discoveries");

  if (!discoverIndexEnsured) {
    await col.createIndex({ updated_at: 1 }, { expireAfterSeconds: 300 });
    await col.createIndex({ user_id: 1 });
    discoverIndexEnsured = true;
  }

  if (req.method === "POST") {
    const { port } = req.body || {};
    if (!port || typeof port !== "number" || port < 1 || port > 65535) {
      return res.status(400).json({ error: "port required (1-65535)" });
    }
    await col.updateOne(
      { user_id: userId, port },
      { $set: { user_id: userId, port, updated_at: new Date() } },
      { upsert: true }
    );
    return res.json({ ok: true });
  }

  if (req.method === "GET") {
    const docs = await col.find({ user_id: userId }).toArray();
    return res.json({ ports: docs.map((d) => d.port) });
  }

  if (req.method === "DELETE") {
    const { port } = req.body || {};
    if (port) {
      await col.deleteOne({ user_id: userId, port });
    } else {
      await col.deleteMany({ user_id: userId });
    }
    return res.json({ ok: true });
  }

  return res.status(405).end();
}
