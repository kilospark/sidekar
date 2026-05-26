import { getUserOrDevice } from "../_auth.js";
import { getDb } from "../_db.js";

const LINK_CODE_TTL_SECS = 600;
const ALPHA = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";

function generateCode() {
  let out = "";
  for (let i = 0; i < 8; i++) out += ALPHA[Math.floor(Math.random() * ALPHA.length)];
  return out;
}

async function handleLink(req, res, user) {
  if (req.method !== "POST") return res.status(405).end();

  const botUsername = process.env.TELEGRAM_BOT_USERNAME || "sidekar_bot";

  try {
    const db = await getDb();
    const coll = db.collection("telegram_link_codes");
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
    if (!code) return res.status(500).json({ error: "code collision" });

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

async function handleStatus(req, res, user) {
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

export default async function handler(req, res) {
  const user = await getUserOrDevice(req);
  if (!user?.user_id) {
    return res.status(401).json({ error: "not authenticated" });
  }

  const action = req.query.action;
  if (action === "link") return handleLink(req, res, user);
  if (action === "status") return handleStatus(req, res, user);
  return res.status(404).json({ error: "not found" });
}
