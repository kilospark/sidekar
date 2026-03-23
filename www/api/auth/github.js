export default async function handler(req, res) {
  if (req.method !== "GET") return res.status(405).end();

  const clientId = process.env.GITHUB_CLIENT_ID;
  if (!clientId) return res.status(500).json({ error: "GITHUB_CLIENT_ID not set" });

  const redirectUri = `https://sidekar.dev/api/auth/github/callback`;
  const scope = "read:user user:email";
  const url = `https://github.com/login/oauth/authorize?client_id=${clientId}&redirect_uri=${encodeURIComponent(redirectUri)}&scope=${encodeURIComponent(scope)}`;

  res.redirect(302, url);
}
