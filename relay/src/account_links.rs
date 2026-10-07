//! Directed grants in MongoDB `account_links`: `grantor_id` → `grantee_id`.
//! Grantee may use grantor resources allowed by the link's `scopes`.

use futures_util::StreamExt;
use mongodb::bson::{doc, oid::ObjectId, Document};
use mongodb::Database;
use std::collections::HashSet;

/// Hex `user_id` strings whose resources `user_hex` may aggregate (including self).
pub async fn expand_linked_user_hex_ids(db: &Database, user_hex: &str) -> Vec<String> {
    expand_linked_user_hex_ids_for_scope(db, user_hex, "sessions").await
}

/// Hex `user_id` strings whose scoped resources `user_hex` may aggregate (including self).
pub async fn expand_linked_user_hex_ids_for_scope(
    db: &Database,
    user_hex: &str,
    scope: &str,
) -> Vec<String> {
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
        if !link_has_scope(&doc, scope) {
            continue;
        }
        let Ok(g) = doc.get_object_id("grantor_id") else {
            continue;
        };
        set.insert(g.to_hex().to_ascii_lowercase());
    }

    set.into_iter().collect()
}

/// Whether `grantee_hex` holds a grant from `grantor_hex` that carries
/// `scope`, or `None` when the database can't say.
pub async fn grant_exists(
    db: &Database,
    grantor_hex: &str,
    grantee_hex: &str,
    scope: &str,
) -> Option<bool> {
    let (Ok(grantor), Ok(grantee)) = (
        ObjectId::parse_str(grantor_hex.trim()),
        ObjectId::parse_str(grantee_hex.trim()),
    ) else {
        // Not account ids, so no grant can name them.
        return Some(false);
    };
    let link = db
        .collection::<Document>("account_links")
        .find_one(doc! { "grantor_id": grantor, "grantee_id": grantee })
        .await
        .ok()?;
    Some(link.is_some_and(|d| link_has_scope(&d, scope)))
}

fn link_has_scope(doc: &Document, scope: &str) -> bool {
    let wanted = scope.trim().to_ascii_lowercase();
    if wanted.is_empty() {
        return true;
    }
    match doc.get_array("scopes") {
        Ok(scopes) => scopes
            .iter()
            .filter_map(|v| v.as_str())
            .any(|s| s.trim().eq_ignore_ascii_case(&wanted)),
        Err(_) => matches!(wanted.as_str(), "sessions" | "devices"),
    }
}
