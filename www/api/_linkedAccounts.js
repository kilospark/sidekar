import { ObjectId } from "mongodb";

const COLL_LINKS = "account_links";

/**
 * User IDs whose devices/sessions the current user may aggregate (always includes self).
 * Directed grant: `grantor_id` → `grantee_id` means the grantee may see the grantor’s resources.
 *
 * @param {import("mongodb").Db} db
 * @param {ObjectId | string} userId
 * @returns {Promise<ObjectId[]>}
 */
export async function expandLinkedUserObjectIds(db, userId) {
  await ensureAccountLinksIndexes(db);
  const uid = userId instanceof ObjectId ? userId : new ObjectId(String(userId));
  const ustr = uid.toString();
  const set = new Set([ustr]);

  const grants = await db.collection(COLL_LINKS).find({ grantee_id: uid }).toArray();
  for (const doc of grants) {
    if (doc.grantor_id) set.add(doc.grantor_id.toString());
  }

  return Array.from(set).map((s) => new ObjectId(s));
}

/**
 * Same as expandLinkedUserObjectIds but 24-char hex strings (matches relay `sessions.user_id`).
 * @param {import("mongodb").Db} db
 * @param {string} userIdStr — JWT `sub` / ObjectId string
 */
export async function expandLinkedUserHexIds(db, userIdStr) {
  const oids = await expandLinkedUserObjectIds(db, userIdStr);
  return oids.map((o) => o.toString().toLowerCase());
}

let indexesEnsured = false;

export async function ensureAccountLinksIndexes(db) {
  if (indexesEnsured) return;
  const links = db.collection(COLL_LINKS);
  await links.createIndex(
    { grantor_id: 1, grantee_id: 1 },
    {
      unique: true,
      partialFilterExpression: { grantor_id: { $exists: true } },
    }
  );
  await links.createIndex({ grantee_id: 1 });
  await links.createIndex({ grantor_id: 1 });

  const invites = db.collection("account_link_invites");
  await invites.createIndex({ code: 1 }, { unique: true });
  await invites.createIndex({ expires_at: 1 }, { expireAfterSeconds: 0 });
  indexesEnsured = true;
}
