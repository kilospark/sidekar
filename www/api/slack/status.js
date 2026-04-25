// GET  /api/slack/status  — returns the current channel bindings (if any)
// POST /api/slack/status?unlink=1 — delete all bindings for this user

import { getUserOrDevice } from "../_auth.js";
import { getDb } from "../_db.js";

export default async function handler(req, res) {
  const user = await getUserOrDevice(req);
  if (!user?.user_id) {
    return res.status(401).json({ error: "not authenticated" });
  }

  const db = await getDb();
  const channels = db.collection("slack_channels");

  if (req.method === "POST" && "unlink" in (req.query || {})) {
    await channels.deleteMany({ user_id: user.user_id });
    return res.status(200).json({ ok: true });
  }

  if (req.method !== "GET") return res.status(405).end();

  const docs = await channels.find({ user_id: user.user_id }).toArray();
  const bindings = docs.map((d) => ({
    channel: d.channel,
    session_id: d.session_id || null,
    created_at: d.created_at || null,
    updated_at: d.updated_at || null,
  }));
  return res.status(200).json({ bindings });
}
