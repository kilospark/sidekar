#!/usr/bin/env node
// One-time database initialization: creates indexes for all collections.
// Usage: MONGODB_URI=mongodb+srv://... node scripts/init-db.js

import { MongoClient } from "mongodb";

const MONGODB_URI = process.env.MONGODB_URI || "mongodb://localhost:27017";
const DB_NAME = "sidekar";

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

// Users indexes
await db.collection("users").createIndex({ github_id: 1 }, { unique: true });
await db.collection("users").createIndex({ login: 1 });
console.log("  users: github_id (unique), login");

// Devices indexes
await db.collection("devices").createIndex({ user_id: 1 });
await db.collection("devices").createIndex({ token_hash: 1 }, { unique: true });
console.log("  devices: user_id, token_hash (unique)");

// Device codes indexes (TTL: auto-delete after expires_at)
await db.collection("device_codes").createIndex({ expires_at: 1 }, { expireAfterSeconds: 0 });
await db.collection("device_codes").createIndex({ device_code: 1 }, { unique: true });
await db.collection("device_codes").createIndex({ user_code: 1 });
console.log("  device_codes: expires_at (TTL), device_code (unique), user_code");

await client.close();
console.log("Done.");
