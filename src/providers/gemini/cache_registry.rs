//! Local registry of Gemini `cachedContents` objects keyed by
//! fingerprint.
//!
//! Backed by sidekar's KV store (`sidekar kv`) under the
//! `gemini_cache:<fingerprint_hex>` key, so cache associations
//! persist across REPL restarts and can be inspected with
//! `sidekar kv list gemini_cache`.
//!
//! The fingerprint is a deterministic hash of
//! `(model, system_prompt, tools_json, messages_prefix_json)`. Same
//! bytes in → same fingerprint out. Any change in message prefix
//! (user editing mid-history, new tools added, system prompt
//! refinement) produces a new fingerprint; the old cache is left to
//! expire naturally via its TTL.
//!
//! Why hash instead of storing the raw prefix: fingerprints are
//! small, SHA-256-stable, and don't leak prompt contents into KV
//! logs. We DO need to reconstruct the exact prefix on cache-miss to
//! create a new cache, but the caller already has it in scope — no
//! need to round-trip it through storage.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Table of contents for one cached content entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    /// Gemini's full name, e.g. `"cachedContents/abc123"`.
    pub name: String,
    /// Model the cache was created for. Caches are model-scoped on
    /// the server (Flash caches can't be reused on Pro); we mirror
    /// that by including the model in the fingerprint, but we store
    /// it redundantly here so `sidekar kv list` output is
    /// human-debuggable.
    pub model: String,
    /// Raw fingerprint hex (redundant with the KV key suffix; kept
    /// for debugging and migration).
    pub fingerprint: String,
    /// Cached prefix size as reported by the server on creation.
    /// Used for deciding worth-reusing heuristics later; for now
    /// just informational.
    pub token_count: u64,
    /// Unix-seconds timestamp at which the cache expires. When
    /// lookups find `now >= expires_at_unix`, the entry is treated
    /// as a miss and evicted from KV.
    pub expires_at_unix: i64,
}

fn kv_key(fingerprint: &str) -> String {
    format!("gemini_cache:{fingerprint}")
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Compute a stable fingerprint for a cacheable prefix.
///
/// Input components:
///   - model: caches are model-scoped.
///   - system_prompt: included verbatim.
///   - tools_json: serde_json::to_string of the tools slice. Stable
///     across serialization runs because tool defs are built from
///     the same source each time.
///   - messages_prefix_json: serde_json::to_string of the ChatMessage
///     prefix (everything before the current user turn).
///
/// Algorithm: SHA-256 of `model || 0x00 || system_prompt || 0x00 ||
/// tools_json || 0x00 || messages_prefix_json`, hex-encoded.
pub fn fingerprint(
    model: &str,
    system_prompt: &str,
    tools_json: &str,
    messages_prefix_json: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(model.as_bytes());
    hasher.update([0]);
    hasher.update(system_prompt.as_bytes());
    hasher.update([0]);
    hasher.update(tools_json.as_bytes());
    hasher.update([0]);
    hasher.update(messages_prefix_json.as_bytes());
    let digest = hasher.finalize();
    hex_encode(&digest)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Look up a cache entry by fingerprint. Returns `None` if missing
/// or expired; expired entries are evicted as a side effect so the
/// caller doesn't repeatedly attempt to reuse a dead ID.
pub fn lookup(fingerprint: &str) -> Option<CacheEntry> {
    let key = kv_key(fingerprint);
    let entry = crate::broker::kv_get(&key).ok()??;
    let parsed: CacheEntry = serde_json::from_str(&entry.value).ok()?;
    // Pre-emptive eviction: if we're already past the stored expiry
    // (with a small safety margin to account for clock skew and the
    // generateContent request's travel time), treat as miss.
    const SAFETY_MARGIN_SECS: i64 = 30;
    if now_unix() + SAFETY_MARGIN_SECS >= parsed.expires_at_unix {
        let _ = crate::broker::kv_delete(&key);
        return None;
    }
    Some(parsed)
}

/// Store a new cache entry.
pub fn store(entry: &CacheEntry) -> Result<()> {
    let key = kv_key(&entry.fingerprint);
    let value = serde_json::to_string(entry).context("serialize cache entry")?;
    crate::broker::kv_set(&key, &value, Some(&["gemini_cache".to_string()]))
        .context("kv_set gemini cache entry")?;
    Ok(())
}

/// Remove a cache entry by fingerprint. Called when a
/// `generateContent` request using the cached reference returns
/// "cache not found" — the server evicted it before our TTL
/// believed it was gone, and we should not attempt reuse.
pub fn delete(fingerprint: &str) -> Result<()> {
    let key = kv_key(fingerprint);
    crate::broker::kv_delete(&key).context("kv_delete gemini cache entry")
}

/// Remove all expired entries. Called occasionally (e.g. at REPL
/// start) to keep the KV from accumulating dead rows. Not required
/// for correctness — lookup() evicts lazily.
#[allow(dead_code)]
pub fn evict_expired() -> Result<usize> {
    let entries = crate::broker::kv_list(Some("gemini_cache")).unwrap_or_default();
    let now = now_unix();
    let mut removed = 0;
    for e in entries {
        let parsed: Option<CacheEntry> = serde_json::from_str(&e.value).ok();
        if parsed
            .map(|p| now >= p.expires_at_unix)
            .unwrap_or(true)
        {
            let _ = crate::broker::kv_delete(&e.key);
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_across_calls() {
        let a = fingerprint("gemini-2.5-pro", "sys", "[]", "[]");
        let b = fingerprint("gemini-2.5-pro", "sys", "[]", "[]");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64, "SHA-256 hex is 64 chars");
    }

    #[test]
    fn fingerprint_changes_on_any_input_change() {
        let base = fingerprint("gemini-2.5-pro", "sys", "[]", "[]");
        assert_ne!(
            base,
            fingerprint("gemini-2.5-flash", "sys", "[]", "[]"),
            "model change must change fingerprint"
        );
        assert_ne!(
            base,
            fingerprint("gemini-2.5-pro", "sys2", "[]", "[]"),
            "system prompt change must change fingerprint"
        );
        assert_ne!(
            base,
            fingerprint("gemini-2.5-pro", "sys", "[{\"name\":\"Bash\"}]", "[]"),
            "tools change must change fingerprint"
        );
        assert_ne!(
            base,
            fingerprint("gemini-2.5-pro", "sys", "[]", "[{\"role\":\"user\"}]"),
            "messages change must change fingerprint"
        );
    }

    #[test]
    fn fingerprint_not_sensitive_to_zero_byte_confusion() {
        // The separator is 0x00 specifically to prevent field-shift
        // collisions: without a separator, "a" + "bc" would collide
        // with "ab" + "c". Verify an adversarial input shift changes
        // the fingerprint.
        let a = fingerprint("model", "system", "tools", "messages");
        let b = fingerprint("modelsystem", "toolsmessages", "", "");
        assert_ne!(a, b);
    }
}
