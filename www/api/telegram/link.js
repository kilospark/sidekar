// POST /api/telegram/link
// Mint a one-time Telegram link code for the authenticated user.
// User DMs `/start <code>` to the bot; relay redeems from the shared
// telegram_link_codes collection.

import { getUserOrDevice } from "../_auth.js";
import { getDb } from "../_db.js";

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
  if (req.method !== "POST") return res.status(405).end();

  const user = await getUserOrDevice(req);
  if (!user?.user_id) {
    return res.status(401).json({ error: "not authenticated" });
  }

  const botUsername = process.env.TELEGRAM_BOT_USERNAME || "sidekar_bot";

  try {
    const db = await getDb();
    const coll = db.collection("telegram_link_codes");
    // Best-effort unique index (matches relay's ensure_indexes).
    try {
      await coll.createIndex({ code: 1 }, { unique: true });
    } catch {}

    // Retry a few times in case of a vanishingly rare collision.
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

    return res.status(200).json({
      code,
      bot_username: botUsername,
      deep_link: `https://t.me/${botUsername}?start=${code}`,
      expires_in_secs: LINK_CODE_TTL_SECS,
    });
  } catch (e) {
    console.error("telegram link mint failed", e);
    return res.status(500).json({ error: "db error" });
  }
}
