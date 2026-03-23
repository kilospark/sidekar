import { randomBytes } from "crypto";
import { getDb } from "../_db.js";

export default async function handler(req, res) {
  if (req.method !== "POST") return res.status(405).end();

  const deviceCode = randomBytes(16).toString("hex");
  // User-facing code: 4 chars, dash, 4 chars (uppercase alphanumeric, no ambiguous chars)
  const chars = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // no 0/O/1/I
  let userCode = "";
  for (let i = 0; i < 8; i++) {
    if (i === 4) userCode += "-";
    userCode += chars[randomBytes(1)[0] % chars.length];
  }

  const db = await getDb();
  await db.collection("device_codes").insertOne({
    device_code: deviceCode,
    user_code: userCode,
    user_id: null,
    expires_at: new Date(Date.now() + 15 * 60 * 1000), // 15 min
    created_at: new Date(),
  });

  res.json({
    device_code: deviceCode,
    user_code: userCode,
    verification_uri: "https://sidekar.dev/approve",
    expires_in: 900,
    interval: 5,
  });
}
