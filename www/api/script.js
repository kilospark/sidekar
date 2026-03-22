import { readFileSync } from "fs";
import { join } from "path";

export default async function handler(req, res) {
  if (req.method !== "GET") return res.status(405).end();

  const name = req.query.name;
  if (name !== "install" && name !== "uninstall") {
    return res.status(404).json({ error: "Not found" });
  }

  try {
    const script = readFileSync(join(process.cwd(), `scripts/${name}.sh`), "utf-8");
    res.setHeader("Content-Type", "text/plain; charset=utf-8");
    res.setHeader("Cache-Control", "public, max-age=300");
    res.send(script);
  } catch {
    res.status(500).send(`Script not found: ${name}.sh`);
  }
}
