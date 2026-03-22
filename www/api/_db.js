import { MongoClient } from "mongodb";

const MONGODB_URI = process.env.MONGODB_URI || "mongodb://localhost:27017";
const DB_NAME = "sidekar";

let cached = globalThis.__sidekar_mongo;

if (!cached) {
  cached = globalThis.__sidekar_mongo = { client: null, db: null };
}

export async function getDb() {
  if (cached.db) return cached.db;
  cached.client = new MongoClient(MONGODB_URI);
  await cached.client.connect();
  cached.db = cached.client.db(DB_NAME);
  return cached.db;
}
