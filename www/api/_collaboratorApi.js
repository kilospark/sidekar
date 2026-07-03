import { ObjectId } from "mongodb";
import { getDb } from "./_db.js";
import { DEFAULT_LINK_SCOPES, ensureAccountLinksIndexes, normalizeLinkScopes } from "./_linkedAccounts.js";
import { randomBytes } from "crypto";

const INVITE_TTL_MS = 15 * 60 * 1000;

function packUser(uDoc, unlink_mode, unlink_value, scopes = DEFAULT_LINK_SCOPES) {
  if (!uDoc) return null;
  return {
    id: uDoc._id.toString(),
    login: uDoc.login || null,
    name: uDoc.name || null,
    email: uDoc.email || null,
    scopes: normalizeLinkScopes(scopes),
    unlink_mode,
    unlink_value: unlink_value || uDoc._id.toString(),
  };
}

function pushUniqueById(arr, row) {
  if (!row || arr.some((x) => x.id === row.id)) return;
  arr.push(row);
}

/**
 * Collaborator grants (JWT user only). Used from `/api/auth/session?collaborators`.
 *
 * GET — list `{ can_see, can_see_you }`
 * PUT — JSON `{ action: "invite"|"accept", code? }`
 * DELETE — `?grantee=id` | `?grantor=id`
 */
export async function handleCollaboratorsRequest(req, res, jwtSub) {
  const selfId = new ObjectId(jwtSub);

  const db = await getDb();
  await ensureAccountLinksIndexes(db);

  if (req.method === "GET") {
    const outgoing = await db
      .collection("account_links")
      .find({ grantor_id: selfId })
      .toArray();
    const incoming = await db
      .collection("account_links")
      .find({ grantee_id: selfId })
      .toArray();

    const idSet = new Set();
    for (const d of outgoing) {
      if (d.grantee_id) idSet.add(d.grantee_id.toString());
    }
    for (const d of incoming) {
      if (d.grantor_id) idSet.add(d.grantor_id.toString());
    }

    const ids = [...idSet].map((s) => new ObjectId(s));
    const users =
      ids.length === 0
        ? []
        : await db
            .collection("users")
            .find({ _id: { $in: ids } })
            .project({ login: 1, name: 1, email: 1 })
            .toArray();
    const byIdStr = Object.fromEntries(users.map((u) => [u._id.toString(), u]));

    const can_see_you = [];
    for (const d of outgoing) {
      if (!d.grantee_id) continue;
      const uid = d.grantee_id.toString();
      const row = packUser(byIdStr[uid], "grantee", uid, d.scopes);
      pushUniqueById(can_see_you, row);
    }

    const can_see = [];
    for (const d of incoming) {
      if (!d.grantor_id) continue;
      const uid = d.grantor_id.toString();
      const row = packUser(byIdStr[uid], "grantor", uid, d.scopes);
      pushUniqueById(can_see, row);
    }

    return res.json({ can_see, can_see_you });
  }

  if (req.method === "PUT") {
    let body = req.body || {};
    if (typeof body === "string") {
      try {
        body = JSON.parse(body || "{}");
      } catch {
        body = {};
      }
    }
    if (!body || typeof body !== "object") body = {};
    const action = body.action || req.query.action;

    if (action === "invite") {
      const code = randomBytes(5).toString("hex").slice(0, 10);
      const expires_at = new Date(Date.now() + INVITE_TTL_MS);
      const scopes = normalizeLinkScopes(body.scopes);
      await db.collection("account_link_invites").insertOne({
        code,
        from_user_id: selfId,
        scopes,
        expires_at,
        created_at: new Date(),
      });
      return res.json({ code, scopes, expires_at: expires_at.toISOString() });
    }

    if (action === "accept") {
      const code = (body.code || "").trim().toLowerCase();
      if (!code) {
        return res.status(400).json({ error: "code required" });
      }

      const invite = await db.collection("account_link_invites").findOne({ code });
      if (!invite || invite.expires_at < new Date()) {
        return res.status(400).json({ error: "invalid or expired code" });
      }

      const grantorId = invite.from_user_id;
      if (grantorId.equals(selfId)) {
        return res.status(400).json({ error: "cannot accept your own invite" });
      }

      await db.collection("account_links").updateOne(
        { grantor_id: grantorId, grantee_id: selfId },
        {
          $set: {
            scopes: normalizeLinkScopes(invite.scopes),
          },
          $setOnInsert: {
            grantor_id: grantorId,
            grantee_id: selfId,
            created_at: new Date(),
          },
        },
        { upsert: true }
      );

      await db.collection("account_link_invites").deleteOne({ code });
      return res.json({ ok: true });
    }

    return res.status(400).json({ error: "unknown action" });
  }

  if (req.method === "DELETE") {
    const grantee = req.query.grantee;
    const grantor = req.query.grantor;

    if (grantee && ObjectId.isValid(grantee)) {
      const r = await db.collection("account_links").deleteOne({
        grantor_id: selfId,
        grantee_id: new ObjectId(grantee),
      });
      if (r.deletedCount > 0) return res.json({ ok: true });
      return res.status(404).json({ error: "grant not found" });
    }

    if (grantor && ObjectId.isValid(grantor)) {
      const r = await db.collection("account_links").deleteOne({
        grantor_id: new ObjectId(grantor),
        grantee_id: selfId,
      });
      if (r.deletedCount > 0) return res.json({ ok: true });
      return res.status(404).json({ error: "grant not found" });
    }

    return res.status(400).json({ error: "use grantee=id or grantor=id query params" });
  }

  return res.status(405).end();
}
