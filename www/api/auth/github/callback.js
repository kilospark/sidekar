import { getDb } from "../../_db.js";
import { signToken, setSessionCookie } from "../../_auth.js";

export default async function handler(req, res) {
  if (req.method !== "GET") return res.status(405).end();

  const { code } = req.query;
  if (!code) return res.status(400).json({ error: "missing code" });

  // Exchange code for access token
  const tokenRes = await fetch("https://github.com/login/oauth/access_token", {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify({
      client_id: process.env.GITHUB_CLIENT_ID,
      client_secret: process.env.GITHUB_CLIENT_SECRET,
      code,
    }),
  });
  const tokenData = await tokenRes.json();
  if (tokenData.error) return res.status(400).json({ error: tokenData.error_description });

  // Fetch user profile
  const userRes = await fetch("https://api.github.com/user", {
    headers: { Authorization: `Bearer ${tokenData.access_token}`, Accept: "application/json" },
  });
  const ghUser = await userRes.json();

  // Fetch primary email if not public
  let email = ghUser.email;
  if (!email) {
    const emailsRes = await fetch("https://api.github.com/user/emails", {
      headers: { Authorization: `Bearer ${tokenData.access_token}`, Accept: "application/json" },
    });
    const emails = await emailsRes.json();
    const primary = emails.find((e) => e.primary && e.verified);
    email = primary ? primary.email : null;
  }

  // Upsert user
  const db = await getDb();
  const result = await db.collection("users").findOneAndUpdate(
    { github_id: ghUser.id },
    {
      $set: {
        login: ghUser.login,
        name: ghUser.name || ghUser.login,
        email,
        avatar_url: ghUser.avatar_url,
        last_login_at: new Date(),
      },
      $setOnInsert: { created_at: new Date() },
    },
    { upsert: true, returnDocument: "after" }
  );

  const user = result;
  const jwt = await signToken({
    sub: user._id.toString(),
    login: user.login,
    name: user.name,
  });

  setSessionCookie(res, jwt);
  res.redirect(302, "/sessions");
}
