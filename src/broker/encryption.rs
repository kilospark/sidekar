use super::*;
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use base64::Engine;
use rand::Rng;
use std::sync::Mutex;

static ENCRYPTION_KEY: Mutex<Option<Vec<u8>>> = Mutex::new(None);

/// Set once the *account* key has been fetched in this process.
///
/// Recorded outright rather than inferred, because both obvious proxies are
/// wrong. A user id is known without the key: it is hydrated from disk on
/// first read. And a key can be loaded that is not the account's:
/// `ensure_local_key` installs a local one before login. `ensure_account_key`
/// once used the first proxy, and when uid hydration landed it began returning
/// early on every logged-in machine without ever fetching — so credential
/// reads came back as ciphertext and failed as "unknown credential".
static ACCOUNT_KEY_LOADED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// True once this process holds the account key, not merely a local one.
pub fn account_key_loaded() -> bool {
    ACCOUNT_KEY_LOADED.load(std::sync::atomic::Ordering::SeqCst)
}

/// In-memory cache of the current uid for this process. Wrapped in an outer
/// `Option` so "never looked at disk yet" (`None`) is distinguishable from
/// "looked, and there is no logged-in user" (`Some(None)`) -- otherwise every
/// call after the first empty read would re-hit the database instead of
/// caching the negative result.
static CURRENT_USER_ID: Mutex<Option<Option<String>>> = Mutex::new(None);

/// Key under which the last-seen uid is persisted in `encryption_meta`, so a
/// fresh CLI process can tell "already logged in as this account" (skip
/// migration) apart from "never logged in" (`old_uid = ""`). Without this,
/// every invocation's process-local static started empty and looked like a
/// fresh login transition even when nothing had changed since the last run
/// (see `fetch_encryption_key`, issue #11).
const CURRENT_USER_ID_META_KEY: &str = "current_user_id_v1";

pub fn set_encryption_key(key: Vec<u8>) {
    let mut guard = ENCRYPTION_KEY.lock().unwrap();
    *guard = Some(key);
}

pub fn clear_encryption_key() {
    let mut guard = ENCRYPTION_KEY.lock().unwrap();
    *guard = None;
    ACCOUNT_KEY_LOADED.store(false, std::sync::atomic::Ordering::SeqCst);
}

pub fn get_encryption_key() -> Option<Vec<u8>> {
    ENCRYPTION_KEY.lock().unwrap().clone()
}

pub fn set_current_user_id(user_id: String) {
    persist_current_user_id(Some(&user_id));
    *CURRENT_USER_ID.lock().unwrap() = Some(Some(user_id));
}

pub fn clear_current_user_id() {
    persist_current_user_id(None);
    *CURRENT_USER_ID.lock().unwrap() = Some(None);
}

/// Resolve the current uid, hydrating the in-memory cache from
/// `encryption_meta` on first use in this process if it hasn't been set yet.
pub fn current_user_id() -> Option<String> {
    let mut guard = CURRENT_USER_ID.lock().unwrap();
    if let Some(ref cached) = *guard {
        return cached.clone();
    }
    let persisted = read_persisted_current_user_id();
    *guard = Some(persisted.clone());
    persisted
}

/// Best-effort: a failure to persist must not block login/logout, it just
/// means the next process re-derives `old_uid = ""` and re-runs migration
/// once more, same as before this fix existed.
fn persist_current_user_id(uid: Option<&str>) {
    if let Err(e) = persist_current_user_id_inner(uid) {
        crate::broker::try_log_event(
            "warn",
            "encryption",
            "failed to persist current user id",
            Some(&format!("{e:#}")),
        );
    }
}

fn persist_current_user_id_inner(uid: Option<&str>) -> Result<()> {
    let conn = open()?;
    match uid {
        Some(uid) => conn.execute(
            "INSERT INTO encryption_meta (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![CURRENT_USER_ID_META_KEY, uid],
        ),
        None => conn.execute(
            "DELETE FROM encryption_meta WHERE key = ?1",
            params![CURRENT_USER_ID_META_KEY],
        ),
    }?;
    Ok(())
}

/// Stand in for a successful account fetch, which needs the network.
#[cfg(test)]
pub(crate) fn mark_account_key_loaded_for_test() {
    ACCOUNT_KEY_LOADED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Forget the in-memory uid cache without touching what is persisted on
/// disk, simulating a fresh CLI process starting up against a database that
/// already recorded a prior login. Test-only: production code never needs
/// to un-hydrate itself mid-process.
#[cfg(test)]
pub(crate) fn reset_current_user_id_cache_for_test() {
    *CURRENT_USER_ID.lock().unwrap() = None;
}

fn read_persisted_current_user_id() -> Option<String> {
    let conn = open().ok()?;
    conn.query_row(
        "SELECT value FROM encryption_meta WHERE key = ?1",
        params![CURRENT_USER_ID_META_KEY],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
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

/// Envelope prefix for records pushed to the server-side sync store. Distinct
/// from `$encrypted$` on purpose: a sync blob must never be readable through
/// the local KV/TOTP decrypt path (`is_encrypted`/`decrypt`) or vice versa,
/// so the two ciphertext families can't be confused for one another.
const SYNC_PREFIX: &str = "$sync1$";

/// Encrypt `plaintext` for the sync store under the `$sync1$` envelope.
pub(crate) fn sync_encrypt(key: &[u8], plaintext: &str) -> Result<String> {
    let local = encrypt_with_key(plaintext, key)?;
    let data = local
        .strip_prefix("$encrypted$")
        .context("internal: encrypt_with_key produced an unexpected envelope")?;
    Ok(format!("{SYNC_PREFIX}{data}"))
}

/// Decrypt a `$sync1$` blob pulled from the sync store. Rejects anything not
/// under that prefix (including plain `$encrypted$` values) instead of
/// silently reinterpreting it.
pub(crate) fn sync_decrypt(key: &[u8], blob: &str) -> Result<String> {
    let data = blob
        .strip_prefix(SYNC_PREFIX)
        .context("sync ciphertext missing $sync1$ prefix")?;
    decrypt_with_key(&format!("$encrypted${data}"), key)
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
/// Make sure the account key is loaded, fetching it once if it is not.
///
/// `main` fetches this before dispatch for most commands but deliberately skips
/// the ones meant to stay fast and offline — `config`, `memory`, `tasks` and
/// friends. Two of those turned out to need user-scoped KV anyway: `memory
/// import` reads the credential it extracts with, and `config set credential`
/// checks the name is real. Both failed with "unknown credential" on a
/// logged-in machine, because KV rows are scoped by a `user_id` that only this
/// fetch installs.
///
/// Rather than grow the skip list into a list of exceptions to itself, the
/// credential path asks for the key where it needs it. Idempotent: once the key
/// is loaded this is a mutex read, and it never fires for a machine that is not
/// logged in.
pub async fn ensure_account_key() -> Result<()> {
    if account_key_loaded() || crate::auth::auth_token().is_none() {
        return Ok(());
    }
    fetch_encryption_key().await?;
    Ok(())
}

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
    ACCOUNT_KEY_LOADED.store(true, std::sync::atomic::Ordering::SeqCst);

    if let Some(ref uid) = body.user_id {
        if old_uid != *uid {
            // Rows written before login (or under a different account) live
            // under a different user_id and would otherwise stay invisible,
            // still encrypted under the local key we just replaced above, or
            // (if written before any key existed) in plaintext. Migrate
            // *before* persisting the new uid: if migration fails, the next
            // process must still see `old_uid` and retry the transition
            // instead of treating it as already done.
            migrate_login_transition(&old_uid, uid)?;
        }
        set_current_user_id(uid.clone());
    }

    Ok(Some(decoded))
}
