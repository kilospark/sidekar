import { getUser, parseCookie } from "./_auth.js";

const RELAY_URL = (process.env.RELAY_URL || "https://relay.sidekar.dev").trim();

export default async function handler(req, res) {
  if (req.method !== "GET") return res.status(405).end();

  const user = await getUser(req);
  if (!user) return res.status(401).json({ error: "not authenticated" });

  const jwt = parseCookie(req);

  try {
    const url = `${RELAY_URL}/sessions?token=${encodeURIComponent(jwt)}`;
    const relayRes = await fetch(url);
    const text = await relayRes.text();

    if (!relayRes.ok) {
      return res.status(502).json({ error: "relay error", status: relayRes.status, body: text, relay_url: RELAY_URL });
    }

    const data = JSON.parse(text);
    res.json(data);
  } catch (err) {
    res.status(500).json({ error: err.message, relay_url: RELAY_URL });
  }
}
