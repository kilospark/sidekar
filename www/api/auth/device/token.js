import { getDb } from "../../_db.js";

export default async function handler(req, res) {
  if (req.method !== "POST") return res.status(405).end();

  const { device_code } = req.body;
  if (!device_code) return res.status(400).json({ error: "device_code required" });

  const db = await getDb();
  const doc = await db.collection("device_codes").findOne({ device_code });

  if (!doc) return res.status(404).json({ error: "expired" });

  if (!doc.user_id) {
    // Not yet approved
    return res.json({ status: "pending" });
  }

  // Approved — return token and delete the device code
  const token = doc.token;
  await db.collection("device_codes").deleteOne({ _id: doc._id });

  res.json({ status: "approved", token });
}
