import { getUserOrDevice } from "./_auth.js";
import { getDb } from "./_db.js";

const SESSION_TTL_MS = 90 * 1000; // matches relay's SESSION_TTL_SECS

export default async function handler(req, res) {
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
    }));

    res.json({ sessions });
  } catch (err) {
    console.error("sessions list failed:", err);
    return res.status(500).json({ error: "failed to load sessions" });
  }
}
