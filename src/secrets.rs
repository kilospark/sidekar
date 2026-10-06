//! Grant-aware secret access facade.
//!
//! Secret access goes through this module so local broker reads and linked-account
//! remote reads keep one reference syntax and one authorization boundary.

use crate::*;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretOwner {
    Local,
    Remote { label: String },
}

impl SecretOwner {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Local => "local",
            Self::Remote { label } => label.as_str(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretNameRef {
    pub owner: SecretOwner,
    pub name: String,
}

impl SecretNameRef {
    pub fn parse(raw: &str) -> Self {
        match raw.split_once('/') {
            Some((owner, name)) if !owner.is_empty() && !name.is_empty() => Self {
                owner: SecretOwner::Remote {
                    label: owner.to_string(),
                },
                name: name.to_string(),
            },
            _ => Self {
                owner: SecretOwner::Local,
                name: raw.to_string(),
            },
        }
    }

    pub fn display(&self) -> String {
        match &self.owner {
            SecretOwner::Local => self.name.clone(),
            SecretOwner::Remote { label } => format!("{label}/{}", self.name),
        }
    }

    pub fn ensure_local(&self, operation: &str) -> Result<()> {
        if self.owner.is_local() {
            Ok(())
        } else {
            bail!(
                "{operation} only supports local secrets for now: {}",
                self.display()
            )
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialListEntry {
    pub name: String,
    pub reference: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub owner: String,
    pub local: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KvListEntry {
    pub key: String,
    pub reference: String,
    pub tags: Vec<String>,
    pub owner: String,
    pub local: bool,
    /// Why the value can't be decrypted, if it can't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreadable: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotpListEntry {
    pub id: Option<i64>,
    pub service: String,
    pub account: String,
    pub reference: String,
    pub algorithm: String,
    pub digits: i32,
    pub period: i32,
    pub owner: String,
    pub local: bool,
    /// `totp` or `hotp`. Absent from devices that predate HOTP, whose
    /// secrets are all TOTP.
    #[serde(default = "default_otp_kind")]
    pub kind: String,
    /// Why the secret can't be decrypted, if it can't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreadable: Option<String>,
}

fn default_otp_kind() -> String {
    crate::broker::KIND_TOTP.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedProviderCredential {
    pub provider_type: String,
    pub api_key: String,
    #[serde(default)]
    pub account_id: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub aws_profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SecretRpcResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    items: Vec<serde_json::Value>,
    #[serde(default)]
    entry: Option<serde_json::Value>,
    #[serde(default)]
    credential: Option<ResolvedProviderCredential>,
    #[serde(default)]
    provider_type: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    code: Option<String>,
}

/// Every local credential. One whose value can't be decrypted is still
/// listed: using it then says why it fails.
pub(crate) fn list_local_credentials() -> Vec<CredentialListEntry> {
    let listing = crate::broker::kv_scan(None).unwrap_or_default();
    listing
        .keys()
        .into_iter()
        .filter_map(|(key, _)| {
            let name = key.strip_prefix("oauth:")?;
            let wire = crate::providers::oauth::resolve_provider_type_for_credential(name)
                .unwrap_or("unknown");
            let provider = crate::providers::oauth::credential_provider_display_label(wire);
            Some(CredentialListEntry {
                name: name.to_string(),
                reference: name.to_string(),
                provider,
                email: crate::providers::oauth::credential_email(name),
                owner: "local".to_string(),
                local: true,
            })
        })
        .collect()
}

pub fn list_credentials() -> Vec<CredentialListEntry> {
    let mut entries = list_local_credentials();
    entries.extend(remote_list("credentials", None).unwrap_or_default());
    entries
}

pub fn resolve_provider_type_for_credential(reference: &str) -> Option<&'static str> {
    let parsed = SecretNameRef::parse(reference);
    if parsed.owner.is_local() {
        return crate::providers::oauth::resolve_provider_type_for_credential(&parsed.name);
    }
    let resp = remote_secret_request(
        &parsed,
        json!({
            "action": "credential_meta",
            "kind": "credentials",
            "name": parsed.name,
        }),
    )
    .ok()?;
    provider_type_static(resp.provider_type.as_deref()?)
}

pub fn credential_email(reference: &str) -> Option<String> {
    let parsed = SecretNameRef::parse(reference);
    if parsed.owner.is_local() {
        return crate::providers::oauth::credential_email(&parsed.name);
    }
    remote_secret_request(
        &parsed,
        json!({
            "action": "credential_meta",
            "kind": "credentials",
            "name": parsed.name,
        }),
    )
    .ok()
    .and_then(|resp| resp.email)
}

/// Every local key, including those whose values can't be decrypted, which
/// are marked rather than left out.
pub(crate) fn list_local_kv(filter_tag: Option<&str>) -> Result<Vec<KvListEntry>> {
    let listing = crate::broker::kv_scan(filter_tag)?;
    let readable = listing.entries.into_iter().map(|e| (e.key, e.tags, None));
    let unreadable = listing
        .unreadable
        .into_iter()
        .map(|u| (u.key, u.tags, Some(u.reason)));
    let mut items: Vec<KvListEntry> = readable
        .chain(unreadable)
        .map(|(key, tags, unreadable)| KvListEntry {
            reference: key.clone(),
            key,
            tags,
            owner: "local".to_string(),
            local: true,
            unreadable,
        })
        .collect();
    items.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(items)
}

pub fn list_kv(filter_tag: Option<&str>) -> Result<Vec<KvListEntry>> {
    let mut entries = list_local_kv(filter_tag)?;
    entries.extend(remote_list("kv", filter_tag)?);
    Ok(entries)
}

pub fn get_kv(reference: &str) -> Result<Option<crate::broker::KvEntry>> {
    let parsed = SecretNameRef::parse(reference);
    if parsed.owner.is_local() {
        return crate::broker::kv_get(&parsed.name);
    }
    let resp = remote_secret_request(
        &parsed,
        json!({
            "action": "kv_get",
            "kind": "kv",
            "key": parsed.name,
        }),
    )?;
    let Some(entry) = resp.entry else {
        return Ok(None);
    };
    let mut out = kv_entry_from_value(&entry, &parsed.display())?;
    out.key = parsed.display();
    Ok(Some(out))
}

pub fn get_kv_for_exec(
    keys: &[String],
    filter_tag: Option<&str>,
) -> Result<Vec<crate::broker::KvEntry>> {
    let mut local_keys = Vec::with_capacity(keys.len());
    let mut remote_keys: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for key in keys {
        let parsed = SecretNameRef::parse(key);
        match parsed.owner {
            SecretOwner::Local => local_keys.push(parsed.name),
            SecretOwner::Remote { label } => {
                remote_keys.entry(label).or_default().push(parsed.name);
            }
        }
    }
    let mut out = crate::broker::kv_get_for_exec(&local_keys, filter_tag)?;
    if crate::auth::auth_token().is_none() {
        return Ok(out);
    }
    if keys.is_empty() {
        out.extend(remote_kv_exec(None, filter_tag)?);
    } else {
        for (owner, owner_keys) in remote_keys {
            out.extend(remote_kv_exec(Some((owner, owner_keys)), filter_tag)?);
        }
    }
    Ok(out)
}

/// Every local one-time-password secret, including those that can't be
/// decrypted, which are marked rather than left out.
pub(crate) fn list_local_totp() -> Result<Vec<TotpListEntry>> {
    let listing = crate::broker::totp_scan()?;
    let readable = listing.secrets.into_iter().map(|s| TotpListEntry {
        id: Some(s.id),
        reference: format!("{} {}", s.service, s.account),
        service: s.service,
        account: s.account,
        algorithm: s.algorithm,
        digits: s.digits,
        period: s.period,
        owner: "local".to_string(),
        local: true,
        kind: s.kind,
        unreadable: None,
    });
    let unreadable = listing.unreadable.into_iter().map(|u| TotpListEntry {
        id: Some(u.id),
        reference: format!("{} {}", u.service, u.account),
        service: u.service,
        account: u.account,
        algorithm: u.algorithm,
        digits: u.digits,
        period: u.period,
        owner: "local".to_string(),
        local: true,
        kind: u.kind,
        unreadable: Some(u.reason),
    });
    let mut items: Vec<TotpListEntry> = readable.chain(unreadable).collect();
    items.sort_by(|a, b| (&a.service, &a.account).cmp(&(&b.service, &b.account)));
    Ok(items)
}

pub fn list_totp() -> Result<Vec<TotpListEntry>> {
    let mut entries = list_local_totp()?;
    entries.extend(remote_list("totp", None)?);
    Ok(entries)
}

pub fn get_totp(service_ref: &str, account: &str) -> Result<Option<crate::broker::TotpSecret>> {
    let parsed = SecretNameRef::parse(service_ref);
    parsed.ensure_local("totp get")?;
    crate::broker::totp_get(&parsed.name, account)
}

pub fn get_totp_code(service_ref: &str, account: &str) -> Result<Option<String>> {
    let parsed = SecretNameRef::parse(service_ref);
    if parsed.owner.is_local() {
        return local_totp_code(&parsed.name, account);
    }
    let resp = remote_secret_request(
        &parsed,
        json!({
            "action": "totp_code",
            "kind": "totp",
            "service": parsed.name,
            "account": account,
        }),
    )?;
    Ok(resp.code)
}

pub async fn resolve_credential_for_provider(
    reference: &str,
) -> Result<ResolvedProviderCredential> {
    let parsed = SecretNameRef::parse(reference);
    if parsed.owner.is_local() {
        // Local credentials live in user-scoped KV, which is unreadable until
        // the account key is loaded. Commands `main` skips that fetch for —
        // `memory import` above all, which the PTY journal handoff runs
        // detached — would otherwise fail here on "unknown credential" for a
        // credential that is plainly stored.
        let _ = crate::broker::ensure_account_key().await;
        return resolve_local_credential_for_provider(&parsed.name).await;
    }
    let resp = remote_secret_request(
        &parsed,
        json!({
            "action": "credential_resolve",
            "kind": "credentials",
            "name": parsed.name,
        }),
    )?;
    resp.credential.ok_or_else(|| {
        anyhow!(
            "remote credential '{}' did not return credential data",
            reference
        )
    })
}

pub async fn refresh_remote_credential(reference: &str) -> Result<String> {
    let parsed = SecretNameRef::parse(reference);
    if parsed.owner.is_local() {
        bail!("refresh_remote_credential called for local credential");
    }
    let resp = remote_secret_request(
        &parsed,
        json!({
            "action": "credential_refresh",
            "kind": "credentials",
            "name": parsed.name,
        }),
    )?;
    resp.credential.map(|c| c.api_key).ok_or_else(|| {
        anyhow!(
            "remote credential '{}' did not return refreshed token",
            reference
        )
    })
}

pub(crate) async fn handle_local_secret_request(v: &serde_json::Value) -> serde_json::Value {
    match handle_local_secret_request_inner(v).await {
        Ok(value) => value,
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    }
}

async fn handle_local_secret_request_inner(v: &serde_json::Value) -> Result<serde_json::Value> {
    let action = v
        .get("action")
        .and_then(|x| x.as_str())
        .context("secret request missing action")?;
    let kind = v
        .get("kind")
        .and_then(|x| x.as_str())
        .context("secret request missing kind")?;
    match (kind, action) {
        ("credentials", "list") => Ok(json!({ "ok": true, "items": list_local_credentials() })),
        ("credentials", "credential_meta") => {
            let name = json_str(v, "name")?;
            Ok(json!({
                "ok": true,
                "provider_type": crate::providers::oauth::resolve_provider_type_for_credential(name),
                "email": crate::providers::oauth::credential_email(name),
            }))
        }
        ("credentials", "credential_resolve") => {
            let name = json_str(v, "name")?;
            let credential = resolve_local_credential_for_provider(name).await?;
            Ok(json!({ "ok": true, "credential": credential }))
        }
        ("credentials", "credential_refresh") => {
            let name = json_str(v, "name")?;
            let api_key = crate::providers::oauth::force_refresh_token(name).await?;
            let mut credential = resolve_local_credential_for_provider(name).await?;
            credential.api_key = api_key;
            Ok(json!({ "ok": true, "credential": credential }))
        }
        ("kv", "list") => {
            let tag = v.get("tag").and_then(|x| x.as_str());
            Ok(json!({ "ok": true, "items": list_local_kv(tag)? }))
        }
        ("kv", "kv_get") => {
            let key = json_str(v, "key")?;
            let entry = crate::broker::kv_get(key)?;
            Ok(json!({ "ok": true, "entry": entry.map(kv_entry_json) }))
        }
        ("kv", "kv_exec") => {
            let keys: Vec<String> = v
                .get("keys")
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let tag = v.get("tag").and_then(|x| x.as_str());
            let entries: Vec<serde_json::Value> = crate::broker::kv_get_for_exec(&keys, tag)?
                .into_iter()
                .map(kv_entry_json)
                .collect();
            Ok(json!({ "ok": true, "items": entries }))
        }
        ("totp", "list") => Ok(json!({ "ok": true, "items": list_local_totp()? })),
        ("totp", "totp_code") => {
            let service = json_str(v, "service")?;
            let account = json_str(v, "account")?;
            Ok(json!({ "ok": true, "code": local_totp_code(service, account)? }))
        }
        _ => bail!("unsupported secret request: {kind}/{action}"),
    }
}

async fn resolve_local_credential_for_provider(name: &str) -> Result<ResolvedProviderCredential> {
    let provider_type = crate::providers::oauth::resolve_provider_type_for_credential(name)
        .ok_or_else(|| anyhow!("unknown credential '{name}'"))?;
    match provider_type {
        "anthropic" => Ok(ResolvedProviderCredential {
            provider_type: provider_type.to_string(),
            api_key: crate::providers::oauth::get_anthropic_token(Some(name)).await?,
            account_id: String::new(),
            base_url: String::new(),
            display_name: String::new(),
            region: None,
            aws_profile: None,
        }),
        "codex" => {
            let (api_key, account_id) =
                crate::providers::oauth::get_codex_token(Some(name)).await?;
            Ok(ResolvedProviderCredential {
                provider_type: provider_type.to_string(),
                api_key,
                account_id,
                base_url: String::new(),
                display_name: String::new(),
                region: None,
                aws_profile: None,
            })
        }
        "openrouter" => local_token_credential(
            provider_type,
            crate::providers::oauth::get_openrouter_token(Some(name)).await?,
        ),
        "opencode-zen" => local_token_credential(
            provider_type,
            crate::providers::oauth::get_opencode_token(Some(name)).await?,
        ),
        "opencode-go" => local_token_credential(
            provider_type,
            crate::providers::oauth::get_opencode_go_token(Some(name)).await?,
        ),
        "grok" => Ok(ResolvedProviderCredential {
            provider_type: provider_type.to_string(),
            api_key: crate::providers::oauth::get_grok_token(Some(name)).await?,
            account_id: String::new(),
            base_url: crate::providers::grok_oauth::grok_repl_base_url(name),
            display_name: String::new(),
            region: None,
            aws_profile: None,
        }),
        "gemini" => local_token_credential(
            provider_type,
            crate::providers::oauth::get_gemini_token(Some(name)).await?,
        ),
        "cursor" => local_token_credential(
            provider_type,
            crate::providers::oauth::get_cursor_token(Some(name)).await?,
        ),
        "bedrock" => {
            let b = crate::providers::oauth::load_bedrock_stored(name)?;
            Ok(ResolvedProviderCredential {
                provider_type: provider_type.to_string(),
                api_key: String::new(),
                account_id: String::new(),
                base_url: String::new(),
                display_name: String::new(),
                region: Some(b.region),
                aws_profile: b.aws_profile,
            })
        }
        "gcp" => {
            let creds = crate::providers::oauth::get_gcp_vertex_credentials(name).await?;
            Ok(ResolvedProviderCredential {
                provider_type: provider_type.to_string(),
                api_key: crate::providers::oauth::resolve_openai_compat_api_key(&creds.api_key)
                    .await?,
                account_id: String::new(),
                base_url: creds.base_url,
                display_name: creds.name,
                region: None,
                aws_profile: None,
            })
        }
        "oac" => {
            let creds = crate::providers::oauth::get_openai_compat_credentials(name).await?;
            Ok(ResolvedProviderCredential {
                provider_type: provider_type.to_string(),
                api_key: crate::providers::oauth::resolve_openai_compat_api_key(&creds.api_key)
                    .await?,
                account_id: String::new(),
                base_url: creds.base_url,
                display_name: creds.name,
                region: None,
                aws_profile: None,
            })
        }
        _ => bail!("Unknown provider type: {provider_type}"),
    }
}

fn local_token_credential(
    provider_type: &str,
    api_key: String,
) -> Result<ResolvedProviderCredential> {
    Ok(ResolvedProviderCredential {
        provider_type: provider_type.to_string(),
        api_key,
        account_id: String::new(),
        base_url: String::new(),
        display_name: String::new(),
        region: None,
        aws_profile: None,
    })
}

/// Clean a user-supplied base32 TOTP secret into the form `base32` accepts.
///
/// Authenticator setup screens print secrets lowercase, in space- or dash-separated
/// groups, and sometimes padded; the RFC4648 decode table matches uppercase
/// unpadded input only and returns `None` for anything else.
pub fn normalize_totp_secret(raw: &str) -> Result<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .flat_map(char::to_uppercase)
        .collect();
    let cleaned = cleaned.trim_end_matches('=');
    if cleaned.is_empty() {
        bail!("Invalid TOTP secret: no base32 characters found");
    }
    if let Some(bad) = cleaned
        .chars()
        .find(|c| !matches!(c, 'A'..='Z' | '2'..='7'))
    {
        bail!("Invalid TOTP secret: '{bad}' is not a base32 character (allowed: A-Z, 2-7)");
    }
    Ok(cleaned.to_string())
}

/// Decode a base32 TOTP secret to key bytes, tolerating authenticator formatting.
pub fn totp_secret_bytes(secret: &str) -> Result<Vec<u8>> {
    let normalized = normalize_totp_secret(secret)?;
    let bytes = totp_rs::Secret::Encoded(normalized)
        .to_bytes()
        .map_err(|e| anyhow!("Invalid TOTP secret: {e}"))?;
    if bytes.len() < 10 {
        bail!(
            "Invalid TOTP secret: too short: need at least 80 bits, got {} bits",
            bytes.len() * 8
        );
    }
    Ok(bytes)
}

/// An HOTP code (RFC 4226) for `counter`.
///
/// TOTP is HOTP of the time divided by the step, so the TOTP generator with
/// a one-second step, given the counter as the time, is exactly HOTP; the
/// RFC 4226 test vectors in the tests hold it to that.
pub fn hotp_code(secret: &str, algorithm: &str, digits: i32, counter: u64) -> Result<String> {
    let algo = match algorithm {
        "SHA256" => totp_rs::Algorithm::SHA256,
        "SHA512" => totp_rs::Algorithm::SHA512,
        _ => totp_rs::Algorithm::SHA1,
    };
    let key = totp_secret_bytes(secret)?;
    let generator =
        totp_rs::TOTP::new_unchecked(algo, digits as usize, 0, 1, key, None, String::new());
    Ok(generator.generate(counter))
}

/// Base32 for raw key bytes — the form secrets are stored in.
pub fn base32_secret(bytes: &[u8]) -> String {
    totp_rs::Secret::Raw(bytes.to_vec()).to_encoded().to_string()
}

/// Decode a hex string to bytes. Duo's activation returns its `hotp_secret`
/// as 32 lowercase hex characters, not base32.
pub fn hex_decode(s: &str) -> Result<Vec<u8>> {
    let chars: Vec<char> = s.trim().chars().collect();
    if chars.is_empty() || chars.len() % 2 != 0 {
        bail!("not a hex string: {} characters", chars.len());
    }
    let nibble = |c: char| -> Result<u8> {
        c.to_digit(16)
            .map(|d| d as u8)
            .ok_or_else(|| anyhow!("'{c}' is not a hex character"))
    };
    chars
        .chunks(2)
        .map(|p| Ok((nibble(p[0])? << 4) | nibble(p[1])?))
        .collect()
}

/// Convert Duo's `hotp_secret` into the base32 form the secret store and
/// [`hotp_code`] use. Duo returns it as a 32-char hex string, which includes
/// 0, 1, 8 and 9 — characters base32 forbids — so it must be hex-decoded to
/// bytes, NOT base32-validated. Re-encoding those bytes as base32 is lossless,
/// so the stored secret yields identical RFC 4226 codes. Falls back to the
/// base32 path only if the value is not valid hex, so a later format change
/// still works.
pub fn duo_hotp_secret_to_base32(raw: &str) -> Result<String> {
    if let Ok(bytes) = hex_decode(raw) {
        if bytes.len() >= 10 {
            return Ok(base32_secret(&bytes));
        }
    }
    normalize_totp_secret(raw)
}

fn local_totp_code(service: &str, account: &str) -> Result<Option<String>> {
    let Some(rec) = crate::broker::totp_get(service, account)? else {
        return Ok(None);
    };
    if rec.kind == crate::broker::KIND_HOTP {
        // Taking a code advances the counter, so it is a change to sync.
        let Some((rec, counter)) = crate::broker::hotp_take(service, account)? else {
            return Ok(None);
        };
        let code = hotp_code(&rec.secret, &rec.algorithm, rec.digits, counter)
            .with_context(|| format!("stored HOTP secret for {service} ({account})"))?;
        crate::commands::push_sync_after_mutation();
        return Ok(Some(code));
    }
    let algo = match rec.algorithm.as_str() {
        "SHA1" => totp_rs::Algorithm::SHA1,
        "SHA256" => totp_rs::Algorithm::SHA256,
        "SHA512" => totp_rs::Algorithm::SHA512,
        _ => totp_rs::Algorithm::SHA1,
    };
    let secret_bytes = totp_secret_bytes(&rec.secret)
        .with_context(|| format!("stored TOTP secret for {service} ({account})"))?;
    let totp = totp_rs::TOTP::new(
        algo,
        rec.digits as usize,
        1,
        rec.period as u64,
        secret_bytes.clone(),
        None,
        rec.account,
    )
    .unwrap_or_else(|_| {
        totp_rs::TOTP::new_unchecked(
            algo,
            rec.digits as usize,
            1,
            rec.period as u64,
            secret_bytes,
            None,
            account.to_string(),
        )
    });
    Ok(Some(totp.generate(crate::message::epoch_secs())))
}

fn remote_list<T>(kind: &str, tag: Option<&str>) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    if crate::auth::auth_token().is_none() {
        return Ok(Vec::new());
    }
    let mut payload = json!({
        "action": "list",
        "kind": kind,
    });
    if let Some(tag) = tag {
        payload["tag"] = tag.into();
    }
    match relay_secret_request(payload) {
        Ok(resp) => parse_remote_items(resp),
        Err(e) => {
            crate::broker::try_log_error(
                "secrets",
                &format!("remote {kind} list failed"),
                Some(&format!("{e:#}")),
            );
            Ok(Vec::new())
        }
    }
}

fn remote_kv_exec(
    owner_keys: Option<(String, Vec<String>)>,
    filter_tag: Option<&str>,
) -> Result<Vec<crate::broker::KvEntry>> {
    let (owner, keys) = owner_keys.unwrap_or_default();
    let mut payload = json!({
        "action": "kv_exec",
        "kind": "kv",
        "keys": keys,
    });
    if !owner.is_empty() {
        payload["owner"] = owner.clone().into();
    }
    if let Some(tag) = filter_tag {
        payload["tag"] = tag.into();
    }
    let resp = relay_secret_request(payload)?;
    let items: Vec<serde_json::Value> = parse_remote_items(resp)?;
    items
        .iter()
        .map(|v| {
            let key = v.get("key").and_then(|x| x.as_str()).unwrap_or_default();
            let fallback = if owner.is_empty() || key.contains('/') {
                key.to_string()
            } else {
                format!("{owner}/{key}")
            };
            let mut entry = kv_entry_from_value(v, &fallback)?;
            if !owner.is_empty() && !entry.key.contains('/') {
                entry.key = format!("{owner}/{}", entry.key);
            }
            Ok(entry)
        })
        .collect()
}

fn remote_secret_request(
    parsed: &SecretNameRef,
    mut payload: serde_json::Value,
) -> Result<SecretRpcResponse> {
    let SecretOwner::Remote { label } = &parsed.owner else {
        bail!("remote_secret_request called with local ref");
    };
    payload["owner"] = label.clone().into();
    relay_secret_request(payload)
}

fn relay_secret_request(payload: serde_json::Value) -> Result<SecretRpcResponse> {
    let token = crate::auth::auth_token()
        .ok_or_else(|| anyhow!("no device token; run: sidekar device login"))?;
    let url = format!(
        "{}/relay/secrets",
        crate::transport::relay_http_base().trim_end_matches('/')
    );
    std::thread::spawn(move || -> Result<SecretRpcResponse> {
        let client = crate::http_client::blocking_client_builder()
            .timeout(Duration::from_secs(20))
            .build()?;
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&payload)
            .send()
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        if !status.is_success() {
            bail!("relay secrets HTTP {status}: {text}");
        }
        let parsed: SecretRpcResponse =
            serde_json::from_str(&text).context("parse relay secrets JSON")?;
        if !parsed.ok {
            bail!(
                "{}",
                parsed
                    .error
                    .clone()
                    .unwrap_or_else(|| "relay secrets request failed".to_string())
            );
        }
        Ok(parsed)
    })
    .join()
    .map_err(|_| anyhow!("relay_secret_request thread panicked"))?
}

fn parse_remote_items<T>(resp: SecretRpcResponse) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    resp.items
        .into_iter()
        .map(|v| serde_json::from_value(v).context("parse remote secret item"))
        .collect()
}

fn kv_entry_json(entry: crate::broker::KvEntry) -> serde_json::Value {
    json!({
        "id": entry.id,
        "key": entry.key,
        "value": entry.value,
        "tags": entry.tags,
        "created_at": entry.created_at,
        "updated_at": entry.updated_at,
    })
}

fn kv_entry_from_value(
    v: &serde_json::Value,
    fallback_key: &str,
) -> Result<crate::broker::KvEntry> {
    Ok(crate::broker::KvEntry {
        id: v.get("id").and_then(|x| x.as_i64()).unwrap_or_default(),
        key: v
            .get("key")
            .and_then(|x| x.as_str())
            .unwrap_or(fallback_key)
            .to_string(),
        value: v
            .get("value")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        tags: v
            .get("tags")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        created_at: v
            .get("created_at")
            .and_then(|x| x.as_u64())
            .unwrap_or_default(),
        updated_at: v
            .get("updated_at")
            .and_then(|x| x.as_u64())
            .unwrap_or_default(),
    })
}

fn provider_type_static(provider_type: &str) -> Option<&'static str> {
    match provider_type {
        "anthropic" => Some("anthropic"),
        "codex" => Some("codex"),
        "openrouter" => Some("openrouter"),
        "opencode-zen" => Some("opencode-zen"),
        "opencode-go" => Some("opencode-go"),
        "grok" => Some("grok"),
        "gemini" => Some("gemini"),
        "cursor" => Some("cursor"),
        "bedrock" => Some("bedrock"),
        "gcp" => Some("gcp"),
        "oac" => Some("oac"),
        _ => None,
    }
}

fn json_str<'a>(v: &'a serde_json::Value, key: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .with_context(|| format!("secret request missing {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_test_home<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = crate::test_home_lock()
            .lock()
            .map_err(|_| anyhow!("failed to lock test HOME mutex"))?;
        let old_home = env::var_os("HOME");
        let temp_home = env::temp_dir().join(format!(
            "sidekar-secrets-home-{}-{}",
            std::process::id(),
            crate::message::epoch_secs()
        ));
        fs::create_dir_all(&temp_home)?;
        // Safety: tests run in-process and this helper restores HOME before returning.
        // HOME first: clearing the user id persists, and doing it outside the
        // scratch HOME cleared the developer's real one.
        unsafe { env::set_var("HOME", &temp_home) };
        crate::broker::clear_current_user_id();
        crate::broker::clear_encryption_key();
        let result = f();
        crate::broker::clear_current_user_id();
        crate::broker::clear_encryption_key();
        match old_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
        let _ = fs::remove_dir_all(&temp_home);
        result
    }

    #[test]
    fn listing_marks_a_value_that_cant_be_decrypted_instead_of_dropping_it() -> Result<()> {
        with_test_home(|| {
            crate::broker::kv_set("broken", "v", None)?;
            // Under another key, as after a half-finished migration.
            crate::broker::set_encryption_key(vec![0x42u8; 32]);
            crate::broker::kv_set("fine", "v", None)?;

            let items = list_local_kv(None)?;
            let marked: Vec<_> = items
                .iter()
                .map(|e| (e.key.as_str(), e.unreadable.is_some()))
                .collect();
            assert_eq!(marked, [("broken", true), ("fine", false)]);
            Ok(())
        })
    }

    #[test]
    fn normalizes_authenticator_formatted_secrets() {
        assert_eq!(
            normalize_totp_secret("5qgfdbjwysyrw2qb").unwrap(),
            "5QGFDBJWYSYRW2QB"
        );
        assert_eq!(
            normalize_totp_secret(" 5qgf dbjw ysyr w2qb ").unwrap(),
            "5QGFDBJWYSYRW2QB"
        );
        assert_eq!(
            normalize_totp_secret("5qgf-dbjw-ysyr-w2qb").unwrap(),
            "5QGFDBJWYSYRW2QB"
        );
        assert_eq!(
            normalize_totp_secret("KRSXG5CTMVRXEZLU======").unwrap(),
            "KRSXG5CTMVRXEZLU"
        );
    }

    #[test]
    fn rejects_non_base32_secrets() {
        assert!(normalize_totp_secret("  ").is_err());
        assert!(normalize_totp_secret("abcd0189").is_err());
    }

    #[test]
    fn lowercase_secret_decodes_to_ten_bytes() {
        let bytes = totp_secret_bytes("5qgfdbjwysyrw2qb").unwrap();
        assert_eq!(bytes.len(), 10);
        assert_eq!(bytes, totp_secret_bytes("5QGFDBJWYSYRW2QB").unwrap());
    }

    #[test]
    fn rejects_secret_below_eighty_bits() {
        let err = totp_secret_bytes("KRSXG5CT").unwrap_err().to_string();
        assert!(err.contains("80 bits"), "unexpected error: {err}");
    }

    #[test]
    fn secret_name_ref_parses_local_and_remote() {
        let local = SecretNameRef::parse("codex");
        assert_eq!(local.owner, SecretOwner::Local);
        assert_eq!(local.name, "codex");
        assert_eq!(local.display(), "codex");

        let remote = SecretNameRef::parse("kb/codex");
        assert_eq!(
            remote.owner,
            SecretOwner::Remote {
                label: "kb".to_string()
            }
        );
        assert_eq!(remote.name, "codex");
        assert_eq!(remote.display(), "kb/codex");
    }

    #[test]
    fn remote_ref_rejected_by_local_only_operation() {
        let remote = SecretNameRef::parse("kb/key");
        let err = remote
            .ensure_local("kv get")
            .expect_err("remote secret must fail local-only gate");
        assert_eq!(
            err.to_string(),
            "kv get only supports local secrets for now: kb/key"
        );
    }

    #[test]
    fn list_credentials_reads_local_oauth_metadata() -> Result<()> {
        with_test_home(|| {
            let creds = crate::providers::oauth::OAuthCredentials {
                access_token: "access".to_string(),
                refresh_token: "refresh".to_string(),
                expires_at: crate::message::epoch_secs() + 3600,
                metadata: json!({
                    "provider_type": "codex",
                    "email": "dev@example.com"
                }),
            };
            crate::broker::kv_set("oauth:work", &serde_json::to_string(&creds)?, None)?;

            let listed = list_credentials();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].name, "work");
            assert_eq!(listed[0].reference, "work");
            assert_eq!(listed[0].provider, "codex (OpenAI OAuth)");
            assert_eq!(listed[0].email.as_deref(), Some("dev@example.com"));
            assert!(listed[0].local);
            assert_eq!(listed[0].owner, "local");
            Ok(())
        })
    }
}

#[cfg(test)]
mod hotp_tests {
    use super::*;
    use crate::commands::duo;

    /// RFC 4226 Appendix D: the published HOTP codes for the ASCII secret
    /// "12345678901234567890", counters 0..=9. These pin the generator.
    #[test]
    fn matches_the_rfc4226_test_vectors() {
        // base32 of the RFC's ASCII secret.
        let secret = base32_secret(b"12345678901234567890");
        let expected = [
            "755224", "287082", "359152", "969429", "338314", "254676", "287922", "162583",
            "399871", "520489",
        ];
        for (counter, want) in expected.iter().enumerate() {
            assert_eq!(
                hotp_code(&secret, "SHA1", 6, counter as u64).unwrap(),
                *want,
                "counter {counter}"
            );
        }
    }

    #[test]
    fn hex_decode_round_trips_and_rejects_bad_input() {
        assert_eq!(hex_decode("48656c6c6f").unwrap(), b"Hello");
        assert_eq!(hex_decode("00FF").unwrap(), vec![0x00, 0xff]); // case-insensitive
        assert!(hex_decode("abc").is_err(), "odd length");
        assert!(hex_decode("zz").is_err(), "non-hex char");
        assert!(hex_decode("").is_err(), "empty");
    }

    /// Regression: Duo's hotp_secret is hex, not base32. Enroll used to
    /// base32-validate it and reject any secret containing 0/1/8/9, AFTER the
    /// single-use activation code was already spent. Decoding hex to bytes and
    /// storing base32 produces correct RFC 4226 codes — proven by feeding the
    /// RFC's own secret in as hex.
    #[test]
    fn a_duo_hex_secret_decodes_to_rfc4226_codes() {
        // hex of the ASCII secret "12345678901234567890".
        let hex = "3132333435363738393031323334353637383930";
        let stored = duo_hotp_secret_to_base32(hex).unwrap();
        assert_eq!(stored, base32_secret(b"12345678901234567890"));
        assert_eq!(hotp_code(&stored, "SHA1", 6, 0).unwrap(), "755224");
        assert_eq!(hotp_code(&stored, "SHA1", 6, 1).unwrap(), "287082");
    }

    /// A real-shaped Duo secret: 32 hex chars (16 bytes) that includes the
    /// very digits base32 forbids. The old enroll path rejected exactly this.
    #[test]
    fn a_32_char_duo_secret_enrolls_without_a_base32_error() {
        let duo = "0123456789abcdef8899aabbccddeeff";
        assert!(normalize_totp_secret(duo).is_err(), "it is not valid base32");
        let stored = duo_hotp_secret_to_base32(duo).unwrap();
        let code = hotp_code(&stored, "SHA1", 6, 0).unwrap();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn a_duo_activation_url_parses_host_and_code() {
        let (host, code) = duo::parse_activation(
            "https://api-abc1234.duosecurity.com/push/v2/activation/DEADBEEFcode1234?foo=bar",
        )
        .unwrap();
        assert_eq!(host, "api-abc1234.duosecurity.com");
        assert_eq!(code, "DEADBEEFcode1234");
        assert!(duo::is_duo_host(&host));
    }

    #[test]
    fn a_non_duo_host_is_refused() {
        assert!(!duo::is_duo_host("evil.example.com"));
        assert!(!duo::is_duo_host("duosecurity.com.evil.com"));
        assert!(duo::is_duo_host("api-x.duosecurity.com"));
    }
}
