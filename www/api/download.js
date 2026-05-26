import { createReadStream, existsSync } from "fs";
import { join } from "path";

const VERSION_RE = /^v\d+\.\d+\.\d+$/;
const ASSET_RE = /^[A-Za-z0-9._-]+$/;

function contentTypeFor(asset) {
  if (asset.endsWith(".tar.gz")) return "application/gzip";
  if (asset.endsWith(".minisig")) return "application/octet-stream";
  return "application/octet-stream";
}

export default async function handler(req, res) {
  if (req.method !== "GET") return res.status(405).end();

  const version = String(req.query.version || "");
  const asset = String(req.query.asset || "");
  if (!VERSION_RE.test(version) || !ASSET_RE.test(asset)) {
    return res.status(400).json({ error: "invalid download path" });
  }

  const localPath = join(process.cwd(), "public", "binaries", version, asset);
  if (existsSync(localPath)) {
    res.setHeader("Content-Type", contentTypeFor(asset));
    createReadStream(localPath).pipe(res);
    return;
  }

  const repo = process.env.GITHUB_REPO || "kilospark/sidekar";
  const redirectUrl = `https://github.com/${repo}/releases/download/${version}/${asset}`;
  res.setHeader("Cache-Control", "public, max-age=300");
  res.redirect(302, redirectUrl);
}
