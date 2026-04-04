import { getDb } from "../_db.js";
import { signToken, setSessionCookie, linkProvider } from "../_auth.js";

export default async function handler(req, res) {
  try {
    if (req.method !== "GET") return res.status(405).end();

    if (req.query.code) return handleCallback(req, res);

    const clientId = (process.env.GITHUB_CLIENT_ID || "").trim();
    if (!clientId) return res.status(500).json({ error: "GITHUB_CLIENT_ID not set" });

    const redirectUri = "https://sidekar.dev/api/auth/github";
    const scope = "read:user user:email";
    const state = req.query.redirect || "/dashboard";
    const url = `https://github.com/login/oauth/authorize?client_id=${clientId}&redirect_uri=${encodeURIComponent(redirectUri)}&scope=${encodeURIComponent(scope)}&state=${encodeURIComponent(state)}`;
    return res.redirect(302, url);
  } catch (err) {
    res.status(500).json({ error: "internal error" });
  }
}

async function handleCallback(req, res) {
  try {
  const { code } = req.query;
  const clientId = (process.env.GITHUB_CLIENT_ID || "").trim();
  const clientSecret = (process.env.GITHUB_CLIENT_SECRET || "").trim();

  const tokenRes = await fetch("https://github.com/login/oauth/access_token", {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify({
      client_id: clientId,
      client_secret: clientSecret,
      code,
    }),
  });
  const tokenData = await tokenRes.json();
  if (tokenData.error) return res.status(400).json({ error: tokenData.error_description || "OAuth token exchange failed" });

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

  const returnTo = req.query.state || "/dashboard";

  if (returnTo === "link" || returnTo === "link-mobile") {
    const result = await linkProvider(db, req, {
      providerIdField: "github_id",
      providerUserId: ghUser.id,
      updateFields: { login: ghUser.login, avatar_url: ghUser.avatar_url },
      providerName: "github",
      isMobile: returnTo === "link-mobile",
    });
    return res.redirect(302, result.redirect);
  }

  // Mobile app: redirect to custom URL scheme instead of setting cookie
  if (returnTo === "mobile") {
    return res.redirect(302, `sidekar://auth/callback?token=${encodeURIComponent(jwt)}`);
  }

  setSessionCookie(res, jwt);
  return res.redirect(302, returnTo);
  } catch (err) {
    return res.status(500).json({ error: "internal error" });
  }
}
