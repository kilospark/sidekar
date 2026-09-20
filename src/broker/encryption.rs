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

/// Read the persisted local key from `encryption_meta` without installing
/// it as the active key or generating one if absent. Login migration uses
/// this to decrypt rows that were written under it before `fetch_encryption_key`
/// replaces the active key with the account key.
pub(crate) fn read_persisted_local_key(conn: &Connection) -> Result<Option<Vec<u8>>> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM encryption_meta WHERE key = ?1",
            params![LOCAL_KEY_META_KEY],
            |r| r.get(0),
        )
        .optional()?;
    stored
        .map(|encoded| {
            base64::engine::general_purpose::STANDARD
                .decode(encoded.trim())
                .context("invalid local encryption key stored in encryption_meta")
        })
        .transpose()
}

pub fn encrypt(plaintext: &str) -> Result<String> {
    let key = get_encryption_key().context("No encryption key set")?;
    encrypt_with_key(plaintext, &key)
}

fn encrypt_with_key(plaintext: &str, key: &[u8]) -> Result<String> {
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
    let key = get_encryption_key().context("No encryption key set")?;
    decrypt_with_key(encrypted, &key)
}

fn decrypt_with_key(encrypted: &str, key: &[u8]) -> Result<String> {
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

/// Re-encrypt a ciphertext value from `old_key` to `new_key`. A no-op for
/// anything that isn't `$encrypted$...` -- login migration also routes
/// legacy-plaintext rows through the same call site, and those are handled
/// by a later re-encrypt-under-the-active-key pass instead.
pub(crate) fn rekey_value(value: &str, old_key: &[u8], new_key: &[u8]) -> Result<String> {
    if !is_encrypted(value) {
        return Ok(value.to_string());
    }
    let plaintext = decrypt_with_key(value, old_key)
        .context("failed to decrypt a row under the persisted local key during login migration")?;
    encrypt_with_key(&plaintext, new_key)
        .context("failed to re-encrypt a row under the account key during login migration")
}

/// Migrate both stores for a login uid transition, then purge the stale
/// persisted local key. Everything it protected has just been re-encrypted
/// under the account key, so nothing depends on it anymore -- leaving it in
/// `encryption_meta` would only risk shadowing or stranding rows if it were
/// ever consulted again (e.g. by `ensure_local_key` after a future logout).
/// Only purges once both migrations succeed, so a failure here never
/// destroys the one key that could still decrypt a not-yet-migrated row.
pub(crate) fn migrate_login_transition(old_uid: &str, new_uid: &str) -> Result<()> {
    if old_uid == new_uid || new_uid.is_empty() {
        return Ok(());
    }
    super::kv_store::migrate_kv_login_transition(old_uid, new_uid)
        .context("failed to migrate kv rows to the logged-in account")?;
    super::totp::migrate_totp_login_transition(old_uid, new_uid)
        .context("failed to migrate totp secrets to the logged-in account")?;
    if let Err(e) = purge_local_key() {
        crate::broker::try_log_event(
            "warn",
            "encryption",
            "failed to purge the stale local key after login migration",
            Some(&format!("{e:#}")),
        );
    }
    Ok(())
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
            // under a different user_id and would otherwise stay invisible,
            // still encrypted under the local key we just replaced above, or
            // (if written before any key existed) in plaintext.
            migrate_login_transition(&old_uid, uid)?;
        }
    }

    Ok(Some(decoded))
}
