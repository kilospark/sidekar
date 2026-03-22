import { readFileSync } from "fs";
import { join } from "path";

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
  if (req.method !== "GET") return res.status(405).end();
  const version = getLatestVersion();
  const current = req.query.current || "";
  res.json({
    latest: version,
    current_is_latest: current === version,
  });
}
