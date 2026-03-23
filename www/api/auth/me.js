import { getUser } from "../_auth.js";

export default async function handler(req, res) {
  if (req.method !== "GET") return res.status(405).end();
  const user = await getUser(req);
  if (!user) return res.status(401).json({ error: "not authenticated" });
  res.json({ user });
}
