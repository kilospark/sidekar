import { getDb } from "../_db.js";
import { getUserOrDevice } from "../_auth.js";

export default async function handler(req, res) {
  const user = await getUserOrDevice(req);
  if (!user) {
    return res.status(401).json({ error: "Not authenticated" });
  }

  const db = await getDb();
  const collection = db.collection("encryption_keys");

  if (req.method === "GET") {
    let keyDoc = await collection.findOne({ user_id: user.user_id });
    
    if (!keyDoc) {
      const crypto = await import("crypto");
      const key = crypto.randomBytes(32).toString("base64");
      
      await collection.insertOne({
        user_id: user.user_id,
        key,
        created_at: new Date(),
        updated_at: new Date(),
      });
      
      return res.json({ key, user_id: user.user_id });
    }

    return res.json({ key: keyDoc.key, user_id: user.user_id });
  }
  
  if (req.method === "DELETE") {
    await collection.deleteOne({ user_id: user.user_id });
    return res.json({ ok: true });
  }

  res.status(405).end();
}
