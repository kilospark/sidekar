import { getUser, parseCookie, clearSessionCookie } from "../_auth.js";
import { getDb } from "../_db.js";

export default async function handler(req, res) {
  if (req.method === "GET") {
    // Authenticated: return linked providers for current user
    if (req.query.linked !== undefined) {
      const jwt = await getUser(req);
      if (!jwt) return res.status(401).json({ error: "not authenticated" });

      const db = await getDb();
      const { ObjectId } = await import("mongodb");
      const user = await db.collection("users").findOne({ _id: new ObjectId(jwt.sub) });
      if (!user) return res.status(404).json({ error: "user not found" });

      const linked = [];
      if (user.github_id) linked.push({ id: "github", name: "GitHub", login: user.login || null });
      if (user.google_id) linked.push({ id: "google", name: "Google", email: user.email || null });

      // Available providers that are NOT yet linked
      const available = [];
      if ((process.env.GITHUB_CLIENT_ID || "").trim() && !user.github_id) {
        available.push({ id: "github", name: "GitHub", url: "/api/auth/github" });
      }
      if ((process.env.GOOGLE_CLIENT_ID || "").trim() && !user.google_id) {
        available.push({ id: "google", name: "Google", url: "/api/auth/google" });
      }

      return res.json({ linked, available, email: user.email || null });
    }

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
