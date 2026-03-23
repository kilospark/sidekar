import { clearSessionCookie } from "../_auth.js";

export default async function handler(req, res) {
  if (req.method !== "POST") return res.status(405).end();
  clearSessionCookie(res);
  res.json({ ok: true });
}
