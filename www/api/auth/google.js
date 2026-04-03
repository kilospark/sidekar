import { getDb } from "../_db.js";
import { signToken, setSessionCookie, getUser, mergeUsers } from "../_auth.js";

export default async function handler(req, res) {
  try {
    if (req.method !== "GET") return res.status(405).end();

    if (req.query.code) return handleCallback(req, res);

    const clientId = (process.env.GOOGLE_CLIENT_ID || "").trim();
    if (!clientId) return res.status(500).json({ error: "GOOGLE_CLIENT_ID not set" });

    const redirectUri = "https://sidekar.dev/api/auth/google";
    const scope = "openid email profile";
    const state = req.query.redirect || "/dashboard";
    const url =
      `https://accounts.google.com/o/oauth2/v2/auth?client_id=${encodeURIComponent(clientId)}` +
      `&redirect_uri=${encodeURIComponent(redirectUri)}` +
      `&response_type=code` +
      `&scope=${encodeURIComponent(scope)}` +
      `&state=${encodeURIComponent(state)}` +
      `&access_type=offline` +
      `&prompt=select_account`;
    return res.redirect(302, url);
  } catch (err) {
    res.status(500).json({ error: "internal error" });
  }
}

async function handleCallback(req, res) {
  try {
    const { code } = req.query;
    const clientId = (process.env.GOOGLE_CLIENT_ID || "").trim();
    const clientSecret = (process.env.GOOGLE_CLIENT_SECRET || "").trim();

    const tokenRes = await fetch("https://oauth2.googleapis.com/token", {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        code,
        client_id: clientId,
        client_secret: clientSecret,
        redirect_uri: "https://sidekar.dev/api/auth/google",
        grant_type: "authorization_code",
      }),
    });
    const tokenData = await tokenRes.json();
    if (tokenData.error) {
      return res.status(400).json({ error: tokenData.error_description || "OAuth token exchange failed" });
    }

    const userRes = await fetch("https://www.googleapis.com/oauth2/v2/userinfo", {
      headers: { Authorization: `Bearer ${tokenData.access_token}` },
    });
    const gUser = await userRes.json();

    const email = gUser.verified_email ? gUser.email : null;
    const name = gUser.name || gUser.email;
    const login = gUser.email ? gUser.email.split("@")[0] : gUser.id;

    const db = await getDb();

    // Try to find existing user by google_id, then fall back to verified email
    let user = await db.collection("users").findOne({ google_id: gUser.id });
    if (!user && email) {
      user = await db.collection("users").findOne({ email });
    }

    if (user) {
      // Update existing user, link google_id if not already set
      user = await db.collection("users").findOneAndUpdate(
        { _id: user._id },
        {
          $set: {
            google_id: gUser.id,
            name: name,
            email: email || user.email,
            avatar_url: gUser.picture || user.avatar_url,
            last_login_at: new Date(),
          },
        },
        { returnDocument: "after" }
      );
    } else {
      // Create new user
      user = await db.collection("users").findOneAndUpdate(
        { google_id: gUser.id },
        {
          $set: {
            login,
            name,
            email,
            avatar_url: gUser.picture || null,
            google_id: gUser.id,
            last_login_at: new Date(),
          },
          $setOnInsert: { created_at: new Date() },
        },
        { upsert: true, returnDocument: "after" }
      );
    }

    const jwt = await signToken({
      sub: user._id.toString(),
      login: user.login || login,
      name: user.name || name,
    });

    const returnTo = req.query.state || "/dashboard";

    // Account linking: user is already logged in and wants to link Google
    if (returnTo === "link" || returnTo === "link-mobile") {
      const currentUser = await getUser(req);
      if (!currentUser && returnTo === "link") {
        return res.redirect(302, "/settings?error=not_authenticated");
      }
      if (currentUser) {
        const { ObjectId } = await import("mongodb");
        const target = await db.collection("users").findOne({ _id: new ObjectId(currentUser.sub) });
        if (target) {
          // Check if this google_id is already on a different account
          const existing = await db.collection("users").findOne({ google_id: gUser.id });
          if (existing && !existing._id.equals(target._id)) {
            // Merge the existing Google account into the current account
            await mergeUsers(db, target, existing);
          }
          // Link google_id to current user
          await db.collection("users").updateOne(
            { _id: target._id },
            { $set: { google_id: gUser.id, email: email || target.email, avatar_url: gUser.picture || target.avatar_url } }
          );
        }
      }
      if (returnTo === "link-mobile") {
        return res.redirect(302, "sidekar://auth/linked?provider=google");
      }
      return res.redirect(302, "/settings?linked=google");
    }

    // Mobile app: redirect to custom URL scheme instead of setting cookie
    if (returnTo === "mobile") {
      return res.redirect(302, `sidekar://auth/callback?token=${encodeURIComponent(jwt)}`);
    }

    setSessionCookie(res, jwt);
    return res.redirect(302, returnTo);
  } catch (err) {
    console.error("google auth callback failed:", err);
    return res.status(500).json({ error: "internal error" });
  }
}
