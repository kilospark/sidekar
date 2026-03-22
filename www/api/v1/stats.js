import { getDb } from "../_db.js";

let cache = null;
let cacheTime = 0;
const CACHE_TTL = 5 * 60 * 1000;

export default async function handler(req, res) {
  if (req.method !== "GET") return res.status(405).end();

  const now = Date.now();
  if (cache && now - cacheTime < CACHE_TTL) return res.json(cache);

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

    cache = { tools: toolLeaderboard, ratings, sessions: sessionCount.length };
    cacheTime = now;

    res.json(cache);
  } catch {
    if (cache) return res.json(cache);
    res.json({ tools: [], ratings: { average: 0, total: 0, distribution: {} }, sessions: 0 });
  }
}
