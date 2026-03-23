import { getDb } from "../_db.js";
import { signToken, setSessionCookie } from "../_auth.js";

export default async function handler(req, res) {
  if (req.method !== "GET") return res.status(405).end();

  // If code is present, this is the OAuth callback
  if (req.query.code) return handleCallback(req, res);

  // Otherwise, redirect to GitHub
  const clientId = process.env.GITHUB_CLIENT_ID;
  if (!clientId) return res.status(500).json({ error: "GITHUB_CLIENT_ID not set" });

  const redirectUri = `https://sidekar.dev/api/auth/github?callback=1`;
  const scope = "read:user user:email";
  const url = `https://github.com/login/oauth/authorize?client_id=${clientId}&redirect_uri=${encodeURIComponent(redirectUri)}&scope=${encodeURIComponent(scope)}`;
  res.redirect(302, url);
}

async function handleCallback(req, res) {
  const { code } = req.query;

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

  const userRes = await fetch("https://api.github.com/user", {
    headers: { Authorization: `Bearer ${tokenData.access_token}`, Accept: "application/json" },
  });
  const ghUser = await userRes.json();

  let email = ghUser.email;
  if (!email) {
    const emailsRes = await fetch("https://api.github.com/user/emails", {
      headers: { Authorization: `Bearer ${tokenData.access_token}`, Accept: "application/json" },
    });
    const emails = await emailsRes.json();
    const primary = emails.find((e) => e.primary && e.verified);
    email = primary ? primary.email : null;
  }

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
