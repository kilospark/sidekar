import { SignJWT, jwtVerify } from "jose";
import { ObjectId } from "mongodb";
import { normalizeLinkScopes } from "./_linkedAccounts.js";

// Align with relay (`relay/src/auth.rs`): HS256 over UTF-8 bytes of this string.
// If Production omits JWT_SECRET, tokens would be signed with the dev fallback below
// while Fly uses a real secret → relay returns invalid_token on /resolve and WS.
const rawJwtSecret = process.env.JWT_SECRET || "";
if (process.env.VERCEL_ENV === "production" && !rawJwtSecret.trim()) {
  throw new Error(
    "[_auth] JWT_SECRET must be set for Vercel Production (same value as Fly JWT_SECRET)."
  );
}
const JWT_SECRET = new TextEncoder().encode(
  (rawJwtSecret.trim() || "dev-secret-change-me")
);
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

/** JWT string from HttpOnly cookie or `Authorization: Bearer` (for relay WS bootstrap). */
export function getRawSessionToken(req) {
  let token = parseCookie(req);
  if (!token) {
    const auth = req.headers.authorization || "";
    if (auth.startsWith("Bearer ")) {
      token = auth.slice(7).trim();
    }
  }
  return token || null;
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
 * Merge sourceUser into targetUser: move what the source owns, copy provider
 * IDs, delete the source. Returns the updated target user document.
 *
 * Not moved: `encryption_keys` and `secret_sync`. Each account's synced
 * secrets are encrypted under its own key, so the source's records cannot
 * simply become the target's; that needs re-encryption, not a new owner.
 */
export async function mergeUsers(db, targetUser, sourceUser) {
  const targetId = targetUser._id instanceof ObjectId ? targetUser._id : new ObjectId(targetUser._id);
  const sourceId = sourceUser._id instanceof ObjectId ? sourceUser._id : new ObjectId(sourceUser._id);

  if (targetId.equals(sourceId)) return targetUser;

  const byHex = (name) =>
    db.collection(name).updateMany(
      { user_id: sourceId.toString() },
      { $set: { user_id: targetId.toString() } }
    );
  // Independent collections, moved in parallel.
  await Promise.all([
    db.collection("devices").updateMany(
      { user_id: sourceId },
      { $set: { user_id: targetId } }
    ),
    byHex("sessions"),
    byHex("ext_tokens"),
    // Chat bindings are unique per channel or chat, never per user, so a new
    // owner cannot collide with one the target already has.
    byHex("slack_channels"),
    byHex("telegram_chats"),
    byHex("slack_link_codes"),
    byHex("telegram_link_codes"),
    db.collection("account_link_invites").updateMany(
      { from_user_id: sourceId },
      { $set: { from_user_id: targetId } }
    ),
  ]);
  await mergeAccountLinks(db, targetId, sourceId);

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
 * Re-point the source's collaborator grants at the target, both those it gave
 * and those it holds. Deleting the source left them naming an account that no
 * longer exists, so merging two logins cut off everyone the source shared
 * with, and everything shared with it.
 *
 * At most one grant links two accounts in each direction. So where the target
 * already has the grant, it keeps the union of both scopes. A grant between
 * the two accounts being merged would become one from the target to itself,
 * and is dropped.
 */
export async function mergeAccountLinks(db, targetId, sourceId) {
  const links = db.collection("account_links");
  for (const [field, other] of [
    ["grantor_id", "grantee_id"],
    ["grantee_id", "grantor_id"],
  ]) {
    const moving = await links.find({ [field]: sourceId }).toArray();
    for (const link of moving) {
      const counterpart = link[other];
      if (!counterpart || counterpart.equals(targetId) || counterpart.equals(sourceId)) {
        await links.deleteOne({ _id: link._id });
        continue;
      }
      const existing = await links.findOne({ [field]: targetId, [other]: counterpart });
      if (existing) {
        const scopes = normalizeLinkScopes([
          ...normalizeLinkScopes(existing.scopes),
          ...normalizeLinkScopes(link.scopes),
        ]);
        await links.updateOne({ _id: existing._id }, { $set: { scopes } });
        await links.deleteOne({ _id: link._id });
      } else {
        await links.updateOne({ _id: link._id }, { $set: { [field]: targetId } });
      }
    }
  }
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
