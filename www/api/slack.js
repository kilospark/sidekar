// GET  /api/slack           — list channel bindings for authed user
// POST /api/slack           — mint a link code
// POST /api/slack?unlink=1  — unlink all channels

import { getUserOrDevice } from "./_auth.js";
import { getDb } from "./_db.js";

const LINK_CODE_TTL_SECS = 600;
const ALPHA = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";

function generateCode() {
  let out = "";
  for (let i = 0; i < 8; i++) {
    out += ALPHA[Math.floor(Math.random() * ALPHA.length)];
  }
  return out;
}

export default async function handler(req, res) {
  const user = await getUserOrDevice(req);
  if (!user?.user_id) {
    return res.status(401).json({ error: "not authenticated" });
  }

  const db = await getDb();

  // POST /api/slack?unlink=1 — unlink all
  if (req.method === "POST" && "unlink" in (req.query || {})) {
    await db.collection("slack_channels").deleteMany({ user_id: user.user_id });
    return res.status(200).json({ ok: true });
  }

  // POST /api/slack — mint link code
  if (req.method === "POST") {
    try {
      const coll = db.collection("slack_link_codes");
      try {
        await coll.createIndex({ code: 1 }, { unique: true });
      } catch {}

      let code = null;
      for (let attempt = 0; attempt < 5; attempt++) {
        const candidate = generateCode();
        try {
          await coll.insertOne({
            code: candidate,
            user_id: user.user_id,
            created_at: new Date(),
          });
          code = candidate;
          break;
        } catch (e) {
          if (e?.code !== 11000) throw e;
        }
      }
      if (!code) {
        return res.status(500).json({ error: "code collision" });
      }
      return res.status(200).json({ code, expires_in_secs: LINK_CODE_TTL_SECS });
    } catch (e) {
      console.error("slack link mint failed", e);
      return res.status(500).json({ error: "db error" });
    }
  }

  // GET /api/slack — list bindings
  if (req.method === "GET") {
    const docs = await db
      .collection("slack_channels")
      .find({ user_id: user.user_id })
      .toArray();
    const bindings = docs.map((d) => ({
      channel: d.channel,
      session_id: d.session_id || null,
      created_at: d.created_at || null,
      updated_at: d.updated_at || null,
    }));
    return res.status(200).json({ bindings });
  }

  return res.status(405).end();
}
