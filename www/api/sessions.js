import { getUser } from "./_auth.js";

const RELAY_URL = process.env.RELAY_URL || "https://sidekar-relay.fly.dev";

export default async function handler(req, res) {
  if (req.method !== "GET") return res.status(405).end();

  const user = await getUser(req);
  if (!user) return res.status(401).json({ error: "not authenticated" });

  try {
    const relayRes = await fetch(`${RELAY_URL}/sessions`, {
      headers: { Cookie: req.headers.cookie },
    });
    if (!relayRes.ok) {
      // Relay returned an error — return empty sessions rather than propagating
      return res.json({ sessions: [] });
    }
    const data = await relayRes.json();
    res.json(data);
  } catch {
    // Relay is down or unreachable — return empty sessions
    res.json({ sessions: [] });
  }
}
