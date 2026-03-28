import { createHash, randomBytes } from "crypto";
import { getDb } from "../_db.js";
import { getUser } from "../_auth.js";

export default async function handler(req, res) {
  if (req.method !== "POST") return res.status(405).end();

  const user = await getUser(req);
  if (!user) return res.status(401).json({ error: "not authenticated" });

  const db = await getDb();
  const token = randomBytes(32).toString("hex");
  const tokenHash = createHash("sha256").update(token).digest("hex");

  // Upsert: one ext token per user (replacing any previous)
  await db.collection("ext_tokens").updateOne(
    { user_id: user.sub },
    {
      $set: {
        token_hash: tokenHash,
        updated_at: new Date(),
      },
      $setOnInsert: { created_at: new Date() },
    },
    { upsert: true }
  );

  res.json({ token });
}
