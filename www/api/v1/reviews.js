import { getDb } from "../_db.js";

// DLP: strip sensitive patterns before sending to client
function sanitize(text) {
  if (!text || typeof text !== "string") return "";
  return text
    // API keys / tokens (generic: long hex, base64, bearer tokens)
    .replace(/\b(sk|pk|api|key|token|secret|password|bearer|ghp|gho|ghs|ghu|github_pat|xox[bpoas]|AKIA|AIza)[_\-]?[A-Za-z0-9\-_\.]{16,}\b/gi, "[REDACTED]")
    // AWS-style keys
    .replace(/\b[A-Z0-9]{20}\b/g, (m) => /[A-Z].*[0-9]|[0-9].*[A-Z]/.test(m) ? "[REDACTED]" : m)
    // Connection strings (mongodb+srv://, postgres://, redis://, mysql://, etc.)
    .replace(/\b(mongodb(\+srv)?|postgres(ql)?|mysql|redis|amqp|mssql):\/\/[^\s'"]+/gi, "[REDACTED]")
    // URLs with credentials (user:pass@host)
    .replace(/https?:\/\/[^:\s]+:[^@\s]+@[^\s'"]+/gi, "[REDACTED]")
    // Email addresses
    .replace(/\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Z]{2,}\b/gi, "[REDACTED]")
    // IP addresses with ports
    .replace(/\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}(:\d+)?\b/g, "[REDACTED]")
    // Phone numbers (US-style)
    .replace(/\b(\+1[\s\-]?)?\(?\d{3}\)?[\s\-]?\d{3}[\s\-]?\d{4}\b/g, "[REDACTED]")
    // SSN-like patterns
    .replace(/\b\d{3}[\-\s]\d{2}[\-\s]\d{4}\b/g, "[REDACTED]")
    // Credit card numbers (13-19 digits, possibly spaced/dashed)
    .replace(/\b\d{4}[\s\-]?\d{4}[\s\-]?\d{4}[\s\-]?\d{1,7}\b/g, "[REDACTED]")
    // Private keys / certs
    .replace(/-----BEGIN\s+[A-Z\s]+-----[\s\S]*?-----END\s+[A-Z\s]+-----/g, "[REDACTED]")
    // Hex strings that look like secrets (32+ chars)
    .replace(/\b[a-f0-9]{32,}\b/gi, "[REDACTED]")
    // Collapse multiple redactions
    .replace(/(\[REDACTED\]\s*){2,}/g, "[REDACTED] ")
    .trim();
}

export default async function handler(req, res) {
  if (req.method !== "GET") return res.status(405).end();

  try {
    const db = await getDb();

    const reviews = await db
      .collection("feedback")
      .find({ rating: { $gte: 1 } })
      .sort({ created_at: -1 })
      .limit(20)
      .project({ _id: 0, rating: 1, comment: 1, version: 1, created_at: 1 })
      .toArray();

    const sanitized = reviews
      .filter((r) => r.comment && r.comment.trim().length > 0)
      .map((r) => ({
        rating: r.rating,
        comment: sanitize(r.comment),
        version: r.version,
        created_at: r.created_at,
      }));

    res.setHeader("Cache-Control", "public, s-maxage=300, stale-while-revalidate=600");
    res.json({ reviews: sanitized });
  } catch (err) {
    console.error("reviews error:", err);
    res.json({ reviews: [] });
  }
}
