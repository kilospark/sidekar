// Run with: node --test test/*.test.js
import { test } from "node:test";
import assert from "node:assert/strict";
import { ObjectId } from "mongodb";
import { mergeUsers } from "../api/_auth.js";

/**
 * Just enough of a MongoDB database for mergeUsers: equality filters and
 * `$set`. An ObjectId matches only an ObjectId, as in Mongo, so a filter on
 * the wrong id type fails here the way it would in production.
 */
function fakeDb(seed) {
  const data = {};
  for (const [name, docs] of Object.entries(seed)) {
    data[name] = docs.map((d) => ({ _id: new ObjectId(), ...d }));
  }
  const same = (want, have) =>
    want instanceof ObjectId ? have instanceof ObjectId && want.equals(have) : want === have;
  const matches = (doc, filter) =>
    Object.entries(filter).every(([k, v]) => doc[k] !== undefined && same(v, doc[k]));
  const collection = (name) => {
    const docs = (data[name] ??= []);
    return {
      async updateMany(filter, { $set }) {
        for (const d of docs) if (matches(d, filter)) Object.assign(d, $set);
      },
      async updateOne(filter, { $set }) {
        const d = docs.find((x) => matches(x, filter));
        if (d) Object.assign(d, $set);
      },
      async deleteOne(filter) {
        const i = docs.findIndex((x) => matches(x, filter));
        if (i >= 0) docs.splice(i, 1);
      },
      async findOne(filter) {
        return docs.find((x) => matches(x, filter)) ?? null;
      },
      find(filter) {
        return { toArray: async () => docs.filter((x) => matches(x, filter)).map((d) => ({ ...d })) };
      },
    };
  };
  return { data, collection };
}

const target = new ObjectId(); // the account kept
const source = new ObjectId(); // the account merged into it
const x = new ObjectId();
const y = new ObjectId();

const name = (id) =>
  ({ [target]: "target", [source]: "source", [x]: "x", [y]: "y" })[id.toString()];
const grants = (db) =>
  db.data.account_links
    .map((l) => `${name(l.grantor_id)}->${name(l.grantee_id)} ${[...l.scopes].sort().join(",")}`)
    .sort();
const users = () => [{ _id: target, login: "t" }, { _id: source, github_id: 7 }];

test("a merge keeps who the source shared with, and what was shared with it", async () => {
  const db = fakeDb({
    users: users(),
    account_links: [
      { grantor_id: source, grantee_id: x, scopes: ["sessions"] },
      { grantor_id: y, grantee_id: source, scopes: ["sessions", "kv"] },
    ],
    account_link_invites: [{ code: "abc123", from_user_id: source }],
  });
  await mergeUsers(db, { _id: target }, { _id: source });

  assert.deepEqual(grants(db), ["target->x sessions", "y->target kv,sessions"]);
  assert.ok(db.data.account_link_invites[0].from_user_id.equals(target), "a pending invite still works");
  assert.deepEqual(db.data.users.map((u) => name(u._id)), ["target"]);
});

test("a grant between the two merged accounts is dropped, not turned on itself", async () => {
  const db = fakeDb({
    users: users(),
    account_links: [
      { grantor_id: target, grantee_id: source, scopes: ["sessions"] },
      { grantor_id: source, grantee_id: target, scopes: ["kv"] },
    ],
  });
  await mergeUsers(db, { _id: target }, { _id: source });
  assert.deepEqual(grants(db), []);
});

test("a grant both accounts had keeps the scopes of both", async () => {
  const db = fakeDb({
    users: users(),
    account_links: [
      { grantor_id: target, grantee_id: x, scopes: ["devices"] },
      { grantor_id: source, grantee_id: x, scopes: ["sessions", "kv"] },
      { grantor_id: x, grantee_id: target, scopes: ["sessions"] },
      // Written before scopes existed: the defaults, sessions and devices.
      { grantor_id: x, grantee_id: source },
    ],
  });
  await mergeUsers(db, { _id: target }, { _id: source });
  assert.deepEqual(grants(db), ["target->x devices,kv,sessions", "x->target devices,sessions"]);
});

test("chat bindings and link codes follow the account", async () => {
  const hex = source.toString();
  const db = fakeDb({
    users: users(),
    slack_channels: [{ user_id: hex, channel: "C1" }],
    telegram_chats: [{ user_id: hex, chat_id: 42 }],
    slack_link_codes: [{ user_id: hex, code: "s1" }],
    telegram_link_codes: [{ user_id: hex, code: "t1" }],
  });
  await mergeUsers(db, { _id: target }, { _id: source });
  for (const coll of ["slack_channels", "telegram_chats", "slack_link_codes", "telegram_link_codes"]) {
    assert.equal(db.data[coll][0].user_id, target.toString(), coll);
  }
});
