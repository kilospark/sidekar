use super::*;
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use base64::Engine;
use rand::Rng;
use std::sync::Mutex;

static ENCRYPTION_KEY: Mutex<Option<Vec<u8>>> = Mutex::new(None);
static CURRENT_USER_ID: Mutex<Option<String>> = Mutex::new(None);

pub fn set_encryption_key(key: Vec<u8>) {
    let mut guard = ENCRYPTION_KEY.lock().unwrap();
    *guard = Some(key);
}

pub fn clear_encryption_key() {
    let mut guard = ENCRYPTION_KEY.lock().unwrap();
    *guard = None;
}

pub fn get_encryption_key() -> Option<Vec<u8>> {
    ENCRYPTION_KEY.lock().unwrap().clone()
}

pub fn set_current_user_id(user_id: String) {
    let mut guard = CURRENT_USER_ID.lock().unwrap();
    *guard = Some(user_id);
}

pub fn clear_current_user_id() {
    let mut guard = CURRENT_USER_ID.lock().unwrap();
    *guard = None;
}

pub fn current_user_id() -> Option<String> {
    CURRENT_USER_ID.lock().unwrap().clone()
}

pub fn is_encrypted(value: &str) -> bool {
    value.starts_with("$encrypted$")
}

/// Key `kv_set`/`totp_add` persist a locally-generated key under in
/// `encryption_meta` when no account key has been fetched yet.
const LOCAL_KEY_META_KEY: &str = "account_data_key_v1";

/// Make sure an encryption key is loaded, generating and persisting a
/// random 256-bit local key on first use if none is loaded yet (in memory)
/// or stored (in `encryption_meta`). Called from `kv_set`/`totp_add` so a
/// write before login is encrypted instead of falling back to plaintext.
pub fn ensure_local_key() -> Result<Vec<u8>> {
    if let Some(key) = get_encryption_key() {
        return Ok(key);
    }

    let conn = open()?;
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM encryption_meta WHERE key = ?1",
            params![LOCAL_KEY_META_KEY],
            |r| r.get(0),
        )
        .optional()?;

    let key = match stored {
        Some(encoded) => base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .context("invalid local encryption key stored in encryption_meta")?,
        None => {
            let bytes: [u8; 32] = rand::rng().random();
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
            conn.execute(
                "INSERT INTO encryption_meta (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = ?2",
                params![LOCAL_KEY_META_KEY, encoded],
            )?;
            bytes.to_vec()
        }
    };

    set_encryption_key(key.clone());
    Ok(key)
}

/// Delete the persisted local data key so a logged-out database is inert
/// without re-linking. Local ciphertext rows remain on disk but unreadable
/// until the key is re-established (fresh local key, or login again).
pub fn purge_local_key() -> Result<()> {
    let conn = open()?;
    conn.execute(
        "DELETE FROM encryption_meta WHERE key = ?1",
        params![LOCAL_KEY_META_KEY],
    )?;
    Ok(())
}

pub fn encrypt(plaintext: &str) -> Result<String> {
    let key = ENCRYPTION_KEY.lock().unwrap();
    let key = key.as_ref().context("No encryption key set")?;
    let cipher = Aes256Gcm::new_from_slice(key)?;

    let nonce_bytes: [u8; 12] = rand::rng().random();
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| anyhow::anyhow!("Encryption failed: {}", e))?;

    let mut combined = nonce_bytes.to_vec();
    combined.extend(ciphertext);

    Ok(format!(
        "$encrypted${}",
        base64::engine::general_purpose::STANDARD.encode(combined)
    ))
}

pub fn decrypt(encrypted: &str) -> Result<String> {
    let key = ENCRYPTION_KEY.lock().unwrap();
    let key = key.as_ref().context("No encryption key set")?;
    let cipher = Aes256Gcm::new_from_slice(key)?;

    let data = encrypted
        .strip_prefix("$encrypted$")
        .context("Invalid encrypted format")?;

    let combined = base64::engine::general_purpose::STANDARD
        .decode(data)
        .context("Invalid base64 in encrypted data")?;

    if combined.len() < 12 {
        anyhow::bail!("Encrypted data too short");
    }

    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("Decryption failed: {}", e))?;

    String::from_utf8(plaintext).map_err(|e| anyhow::anyhow!("Invalid UTF-8: {}", e))
}

/// Get encryption key from server (if logged in) and store in memory
pub async fn fetch_encryption_key() -> Result<Option<Vec<u8>>> {
    let token = crate::auth::auth_token().ok_or_else(|| anyhow::anyhow!("Not logged in"))?;
    let base =
        std::env::var("SIDEKAR_API_URL").unwrap_or_else(|_| "https://sidekar.dev".to_string());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = client
        .get(format!("{}/api/v1/encryption-key", base))
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .context("Failed to fetch encryption key")?;
    if !resp.status().is_success() {
        bail!("Failed to fetch encryption key: HTTP {}", resp.status());
    }
    #[derive(serde::Deserialize)]
    struct KeyResp {
        key: String,
        user_id: Option<String>,
    }
    let body: KeyResp = resp
        .json()
        .await
        .context("Failed to parse encryption key response")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(body.key.trim())
        .context("Invalid encryption key format")?;

    let old_uid = current_user_id().unwrap_or_default();

    set_encryption_key(decoded.clone());

    if let Some(ref uid) = body.user_id {
        set_current_user_id(uid.clone());
        if old_uid != *uid {
            // Rows written before login (or under a different account) live
            // under a different user_id and would otherwise stay invisible
            // and, if written before any key existed, in plaintext.
            super::kv_store::migrate_kv_login_transition(&old_uid, uid)
                .context("failed to migrate kv rows to the logged-in account")?;
            super::totp::migrate_totp_login_transition(&old_uid, uid)
                .context("failed to migrate totp secrets to the logged-in account")?;
        }
    }

    Ok(Some(decoded))
}
