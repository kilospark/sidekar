// GET  /api/telegram/status  — returns the current chat binding (if any)
// POST /api/telegram/status?unlink=1 — delete the binding for this user

import { getUserOrDevice } from "../_auth.js";
import { getDb } from "../_db.js";

export default async function handler(req, res) {
  const user = await getUserOrDevice(req);
  if (!user?.user_id) {
    return res.status(401).json({ error: "not authenticated" });
  }

  const db = await getDb();
  const chats = db.collection("telegram_chats");

  if (req.method === "POST" && "unlink" in (req.query || {})) {
    await chats.deleteMany({ user_id: user.user_id });
    return res.status(200).json({ ok: true });
  }

  if (req.method !== "GET") return res.status(405).end();

  const docs = await chats.find({ user_id: user.user_id }).toArray();
  const bindings = docs.map((d) => ({
    chat_id: d.chat_id,
    session_id: d.session_id || null,
    created_at: d.created_at || null,
    updated_at: d.updated_at || null,
  }));
  return res.status(200).json({
    bot_username: process.env.TELEGRAM_BOT_USERNAME || "sidekar_bot",
    bindings,
  });
}
