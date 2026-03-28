import { getDb } from "../_db.js";

let statsCache = null;
let statsCacheTime = 0;
const CACHE_TTL = 5 * 60 * 1000;

function sanitize(text) {
  if (!text || typeof text !== "string") return "";
  return text
    .replace(/\b(sk|pk|api|key|token|secret|password|bearer|ghp|gho|ghs|ghu|github_pat|xox[bpoas]|AKIA|AIza)[_\-]?[A-Za-z0-9\-_\.]{16,}\b/gi, "[REDACTED]")
    .replace(/\b[A-Z0-9]{20}\b/g, (m) => /[A-Z].*[0-9]|[0-9].*[A-Z]/.test(m) ? "[REDACTED]" : m)
    .replace(/\b(mongodb(\+srv)?|postgres(ql)?|mysql|redis|amqp|mssql):\/\/[^\s'"]+/gi, "[REDACTED]")
    .replace(/https?:\/\/[^:\s]+:[^@\s]+@[^\s'"]+/gi, "[REDACTED]")
    .replace(/\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Z]{2,}\b/gi, "[REDACTED]")
    .replace(/\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}(:\d+)?\b/g, "[REDACTED]")
    .replace(/\b(\+1[\s\-]?)?\(?\d{3}\)?[\s\-]?\d{3}[\s\-]?\d{4}\b/g, "[REDACTED]")
    .replace(/\b\d{3}[\-\s]\d{2}[\-\s]\d{4}\b/g, "[REDACTED]")
    .replace(/\b\d{4}[\s\-]?\d{4}[\s\-]?\d{4}[\s\-]?\d{1,7}\b/g, "[REDACTED]")
    .replace(/-----BEGIN\s+[A-Z\s]+-----[\s\S]*?-----END\s+[A-Z\s]+-----/g, "[REDACTED]")
    .replace(/\b[a-f0-9]{32,}\b/gi, "[REDACTED]")
    .replace(/(\[REDACTED\]\s*){2,}/g, "[REDACTED] ")
    .trim();
}

export default async function handler(req, res) {
  if (req.method !== "GET") return res.status(405).end();

  // ?reviews=1 returns reviews, otherwise returns stats
  if (req.query.reviews) {
    return getReviews(req, res);
  }
  return getStats(req, res);
}

async function getStats(req, res) {
  const now = Date.now();
  if (statsCache && now - statsCacheTime < CACHE_TTL) return res.json(statsCache);

  try {
    const db = await getDb();

    const toolAgg = await db
      .collection("telemetry")
      .aggregate([
        { $project: { tools: { $objectToArray: "$tools" } } },
        { $unwind: "$tools" },
        { $group: { _id: "$tools.k", count: { $sum: "$tools.v" } } },
        { $sort: { count: -1 } },
        { $limit: 20 },
      ])
      .toArray();

    const toolLeaderboard = toolAgg.map((t) => ({ tool: t._id, count: t.count }));

    const ratingAgg = await db
      .collection("feedback")
      .aggregate([
        { $group: { _id: "$rating", count: { $sum: 1 } } },
        { $sort: { _id: 1 } },
      ])
      .toArray();

    const totalRatings = ratingAgg.reduce((sum, r) => sum + r.count, 0);
    const avgRating =
      totalRatings > 0
        ? ratingAgg.reduce((sum, r) => sum + r._id * r.count, 0) / totalRatings
        : 0;

    const ratings = {
      average: Math.round(avgRating * 10) / 10,
      total: totalRatings,
      distribution: Object.fromEntries(ratingAgg.map((r) => [r._id, r.count])),
    };

    const sessionCount = await db.collection("telemetry").distinct("session_id");

    statsCache = { tools: toolLeaderboard, ratings, sessions: sessionCount.length };
    statsCacheTime = now;

    res.json(statsCache);
  } catch {
    if (statsCache) return res.json(statsCache);
    res.json({ tools: [], ratings: { average: 0, total: 0, distribution: {} }, sessions: 0 });
  }
}

async function getReviews(req, res) {
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
