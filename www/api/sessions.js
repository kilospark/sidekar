import { getUser, parseCookie } from "./_auth.js";

const RELAY_URL = (process.env.RELAY_URL || "https://relay.sidekar.dev").trim();

export default async function handler(req, res) {
  if (req.method !== "GET") return res.status(405).end();

  const user = await getUser(req);
  if (!user) return res.status(401).json({ error: "not authenticated" });

  const jwt = parseCookie(req);

  try {
    const relayRes = await fetch(`${RELAY_URL}/sessions?token=${encodeURIComponent(jwt)}`);
    if (!relayRes.ok) {
      return res.json({ sessions: [] });
    }
    const data = await relayRes.json();
    res.json(data);
  } catch {
    res.json({ sessions: [] });
  }
}
