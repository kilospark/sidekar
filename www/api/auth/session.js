import { getUser, parseCookie, clearSessionCookie } from "../_auth.js";

export default async function handler(req, res) {
  if (req.method === "GET") {
    // Public: return available identity providers (no auth required)
    if (req.query.providers !== undefined) {
      const providers = [];
      if ((process.env.GITHUB_CLIENT_ID || "").trim()) {
        providers.push({ id: "github", name: "GitHub", url: "/api/auth/github" });
      }
      if ((process.env.GOOGLE_CLIENT_ID || "").trim()) {
        providers.push({ id: "google", name: "Google", url: "/api/auth/google" });
      }
      return res.json({ providers });
    }

    const user = await getUser(req);
    if (!user) return res.status(401).json({ error: "not authenticated" });

    // If ?ws=1, return the raw JWT for WebSocket auth (cookie is HttpOnly)
    if (req.query.ws) {
      const token = parseCookie(req);
      return res.json({ user, token });
    }

    return res.json({ user });
  }

  if (req.method === "DELETE" || req.method === "POST") {
    clearSessionCookie(res);
    return res.json({ ok: true });
  }

  return res.status(405).end();
}
