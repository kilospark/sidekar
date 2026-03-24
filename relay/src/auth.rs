use mongodb::Database;
use sha2::{Digest, Sha256};

/// Validate a device token by SHA-256 hashing it and looking up in the `devices` collection.
/// Returns the user_id as a hex string if valid.
pub async fn validate_device_token(db: &Database, token: &str) -> Option<String> {
    let hash = {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        hex::encode(hasher.finalize())
    };

    let devices = db.collection::<mongodb::bson::Document>("devices");

    let filter = mongodb::bson::doc! { "token_hash": &hash };
    let update = mongodb::bson::doc! {
        "$set": { "last_seen_at": mongodb::bson::DateTime::now() }
    };

    let doc = devices
        .find_one_and_update(filter, update)
        .return_document(mongodb::options::ReturnDocument::After)
        .await
        .ok()??;

    // user_id is stored as an ObjectId
    let user_id = doc.get_object_id("user_id").ok()?;
    Some(user_id.to_hex())
}

/// Validate a session JWT (HS256) and extract the `sub` claim (user_id).
pub fn validate_session_jwt(jwt: &str, secret: &str) -> Option<String> {
    use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};

    let key = DecodingKey::from_secret(secret.as_bytes());
    let mut validation = Validation::new(Algorithm::HS256);
    // The Vercel API sets sub, login, name — we only need sub
    validation.set_required_spec_claims(&["sub", "exp"]);

    let token_data = decode::<serde_json::Value>(jwt, &key, &validation).ok()?;
    let sub = token_data.claims.get("sub")?.as_str()?;
    Some(sub.to_string())
}
