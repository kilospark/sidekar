import { SignJWT, jwtVerify } from "jose";
import { ObjectId } from "mongodb";

const JWT_SECRET = new TextEncoder().encode((process.env.JWT_SECRET || "dev-secret-change-me").trim());
const COOKIE_NAME = "sidekar_session";

export async function signToken(payload) {
  return new SignJWT(payload)
    .setProtectedHeader({ alg: "HS256" })
    .setExpirationTime("30d")
    .sign(JWT_SECRET);
}

export async function verifyToken(token) {
  try {
    const { payload } = await jwtVerify(token, JWT_SECRET);
    return payload;
  } catch {
    return null;
  }
}

export function parseCookie(req) {
  const header = req.headers.cookie || "";
  const match = header.match(new RegExp(`${COOKIE_NAME}=([^;]+)`));
  return match ? match[1] : null;
}

export async function getUser(req) {
  // Try cookie first
  let token = parseCookie(req);
  if (!token) {
    // Try Authorization header (Bearer token)
    const auth = req.headers.authorization;
    if (auth && auth.startsWith("Bearer ")) {
      token = auth.slice(7);
    }
  }
  if (!token) return null;
  return verifyToken(token);
}

/**
 * Authenticate a request by device token (Bearer header → SHA-256 hash lookup).
 * Returns { user_id } (as string) or null.
 */
export async function getDeviceUser(req) {
  const auth = req.headers.authorization;
  if (!auth || !auth.startsWith("Bearer ")) return null;
  const token = auth.slice(7).trim();
  if (!token) return null;

  const { createHash } = await import("crypto");
  const tokenHash = createHash("sha256").update(token).digest("hex");

  const { getDb } = await import("./_db.js");
  const db = await getDb();
  const device = await db.collection("devices").findOne({ token_hash: tokenHash });
  if (!device) return null;

  // Touch last_seen_at
  await db.collection("devices").updateOne(
    { _id: device._id },
    { $set: { last_seen_at: new Date() } }
  );

  return { user_id: device.user_id.toString() };
}

/**
 * Authenticate a request by extension token (Bearer header → SHA-256 hash lookup in ext_tokens).
 * Returns { user_id } (as string) or null.
 */
export async function getExtUser(req) {
  const auth = req.headers.authorization;
  if (!auth || !auth.startsWith("Bearer ")) return null;
  const token = auth.slice(7).trim();
  if (!token) return null;

  const { createHash } = await import("crypto");
  const tokenHash = createHash("sha256").update(token).digest("hex");

  const { getDb } = await import("./_db.js");
  const db = await getDb();
  const extToken = await db.collection("ext_tokens").findOne({ token_hash: tokenHash });
  if (!extToken) return null;

  return { user_id: extToken.user_id.toString() };
}

/**
 * Authenticate by JWT (cookie or Bearer) first, then device token, then extension token.
 * Returns { user_id } (string) or null.
 */
export async function getUserOrDevice(req) {
  const jwt = await getUser(req);
  if (jwt) return { user_id: jwt.sub || jwt.id };
  const device = await getDeviceUser(req);
  if (device) return device;
  return getExtUser(req);
}

export function setSessionCookie(res, token) {
  res.setHeader("Set-Cookie", `${COOKIE_NAME}=${token}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=${30 * 24 * 60 * 60}`);
}

export function clearSessionCookie(res) {
  res.setHeader("Set-Cookie", `${COOKIE_NAME}=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0`);
}

/**
 * Merge sourceUser into targetUser: move devices, copy provider IDs, delete source.
 * Returns the updated target user document.
 */
export async function mergeUsers(db, targetUser, sourceUser) {
  const targetId = targetUser._id instanceof ObjectId ? targetUser._id : new ObjectId(targetUser._id);
  const sourceId = sourceUser._id instanceof ObjectId ? sourceUser._id : new ObjectId(sourceUser._id);

  if (targetId.equals(sourceId)) return targetUser;

  // Move devices, sessions, and ext_tokens in parallel (independent collections)
  await Promise.all([
    db.collection("devices").updateMany(
      { user_id: sourceId },
      { $set: { user_id: targetId } }
    ),
    db.collection("sessions").updateMany(
      { user_id: sourceId.toString() },
      { $set: { user_id: targetId.toString() } }
    ),
    db.collection("ext_tokens").updateMany(
      { user_id: sourceId.toString() },
      { $set: { user_id: targetId.toString() } }
    ),
  ]);

  // Copy provider IDs and fields from source to target
  const updates = {};
  if (sourceUser.github_id && !targetUser.github_id) updates.github_id = sourceUser.github_id;
  if (sourceUser.google_id && !targetUser.google_id) updates.google_id = sourceUser.google_id;
  if (sourceUser.email && !targetUser.email) updates.email = sourceUser.email;
  if (sourceUser.login && !targetUser.login) updates.login = sourceUser.login;
  if (sourceUser.avatar_url && !targetUser.avatar_url) updates.avatar_url = sourceUser.avatar_url;

  if (Object.keys(updates).length > 0) {
    await db.collection("users").updateOne({ _id: targetId }, { $set: updates });
  }

  // Delete source user
  await db.collection("users").deleteOne({ _id: sourceId });

  // Return updated target
  return db.collection("users").findOne({ _id: targetId });
}

/**
 * Link an OAuth provider to the currently logged-in user.
 * If the provider ID belongs to a different account, merge that account first.
 * Returns { redirect } on success, or { error } if not authenticated.
 */
export async function linkProvider(db, req, { providerIdField, providerUserId, updateFields, providerName, isMobile }) {
  const currentUser = await getUser(req);
  if (!currentUser) {
    return isMobile
      ? { redirect: `sidekar://auth/error?reason=not_authenticated` }
      : { redirect: "/settings?error=not_authenticated" };
  }

  const target = await db.collection("users").findOne({ _id: new ObjectId(currentUser.sub) });
  if (target) {
    const existing = await db.collection("users").findOne({ [providerIdField]: providerUserId });
    if (existing && !existing._id.equals(target._id)) {
      await mergeUsers(db, target, existing);
    }
    await db.collection("users").updateOne(
      { _id: target._id },
      { $set: { [providerIdField]: providerUserId, ...updateFields } }
    );
  }

  return isMobile
    ? { redirect: `sidekar://auth/linked?provider=${providerName}` }
    : { redirect: `/settings?linked=${providerName}` };
}
