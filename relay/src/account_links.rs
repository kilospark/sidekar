//! Directed grants in MongoDB `account_links`: `grantor_id` → `grantee_id`.
//! Grantee may use grantor relay sessions / see grantor devices in the dashboard API.

use futures_util::StreamExt;
use mongodb::bson::{doc, oid::ObjectId, Document};
use mongodb::Database;
use std::collections::HashSet;

/// Hex `user_id` strings whose resources `user_hex` may aggregate (including self).
pub async fn expand_linked_user_hex_ids(db: &Database, user_hex: &str) -> Vec<String> {
    let user_norm = user_hex.trim().to_ascii_lowercase();

    let Ok(oid) = ObjectId::parse_str(&user_norm) else {
        return vec![user_norm];
    };

    let mut set = HashSet::new();
    set.insert(user_norm.clone());

    let coll = db.collection::<Document>("account_links");
    let Ok(mut cursor) = coll.find(doc! { "grantee_id": &oid }).await else {
        return vec![user_norm];
    };

    while let Some(Ok(doc)) = cursor.next().await {
        let Ok(g) = doc.get_object_id("grantor_id") else {
            continue;
        };
        set.insert(g.to_hex().to_ascii_lowercase());
    }

    set.into_iter().collect()
}
