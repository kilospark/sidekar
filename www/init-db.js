#!/usr/bin/env node
// One-time database initialization: creates indexes for all collections.
// Usage: MONGODB_URI=mongodb+srv://... node scripts/init-db.js

import { MongoClient } from "mongodb";

const MONGODB_URI = process.env.MONGODB_URI || "mongodb://localhost:27017";
const DB_NAME = "webact";

const client = new MongoClient(MONGODB_URI);
await client.connect();
const db = client.db(DB_NAME);

console.log(`Connected to ${DB_NAME}`);

// Telemetry indexes
await db.collection("telemetry").createIndex({ created_at: 1 });
await db.collection("telemetry").createIndex({ version: 1 });
console.log("  telemetry: created_at, version");

// Feedback indexes
await db.collection("feedback").createIndex({ created_at: 1 });
await db.collection("feedback").createIndex({ rating: 1 });
await db.collection("feedback").createIndex({ status: 1 });
console.log("  feedback: created_at, rating, status");

await client.close();
console.log("Done.");
