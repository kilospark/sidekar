import { readFileSync } from "fs";
import { join } from "path";
import { getDb } from "../_db.js";
import { getUserOrDevice } from "../_auth.js";

let latest = null;

function getLatestVersion() {
  if (latest) return latest;
  try {
    latest = readFileSync(join(process.cwd(), "version.txt"), "utf-8").trim();
  } catch {
    latest = "0.0.0";
  }
  return latest;
}

export default async function handler(req, res) {
  if (req.query.endpoint === "version") {
    if (req.method !== "GET") return res.status(405).end();
    const version = getLatestVersion();
    const current = req.query.current || "";
    return res.json({
      latest: version,
      current_is_latest: current === version,
    });
  }

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
