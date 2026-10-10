//! Refreshing an OAuth credential that several machines share.
//!
//! `oauth:*` credentials sync like any kv key, and Anthropic, Codex and Grok
//! rotate the refresh token on every refresh. Two machines that refreshed with
//! the same refresh token raced: the loser's token was spent (or the provider
//! revoked the whole family), and its user was told to log in again.
//! So a refresh here:
//!
//! 1. pulls first, and uses a token another machine already refreshed;
//! 2. holds an account-wide lease while refreshing (`broker::claim_lease`), and
//!    while another machine holds it, waits for that machine's token instead;
//! 3. pushes the new token at once, so the others find it;
//! 4. when the refresh fails, pulls again, and retries with a newer refresh
//!    token if one arrived, before asking the user to log in.
//!
//! A per-credential file lock does the same between processes on this
//! machine. Logged out, or against a server without leases, it still pulls
//! nothing, refreshes, and saves, as before. Design: context/oauth-refresh-sync.md.

use super::{OAuthCredentials, load_credentials, save_credentials};
use anyhow::{Context, Result, anyhow};
use std::future::Future;
use std::time::{Duration, Instant};

pub(crate) use crate::broker::LeaseClaim;

/// The sync side of a refresh, apart so tests can stand in for the server.
pub(crate) trait RefreshSync {
    /// Pull other machines' changes into the local store. Best effort.
    async fn pull(&self);
    /// Claim the account-wide lease named `lease_id`.
    async fn claim(&self, lease_id: &str) -> LeaseClaim;
    /// Push local changes now. Best effort.
    async fn push(&self);
}

/// The real sync, for the logged-in account; does nothing logged out.
pub(crate) struct AccountSync {
    uid: Option<String>,
}

impl AccountSync {
    pub(crate) async fn current() -> Self {
        let uid = if crate::auth::auth_token().is_some() {
            let _ = crate::broker::ensure_account_key().await;
            crate::broker::current_user_id().filter(|u| !u.is_empty())
        } else {
            None
        };
        Self { uid }
    }
}

impl RefreshSync for AccountSync {
    async fn pull(&self) {
        if let Some(uid) = &self.uid
            && let Err(e) = crate::broker::pull_merge(uid).await
        {
            crate::broker::try_log_event(
                "warn",
                "oauth",
                "pull before refresh failed",
                Some(&format!("{e:#}")),
            );
        }
    }

    async fn claim(&self, lease_id: &str) -> LeaseClaim {
        if self.uid.is_none() {
            return LeaseClaim::Unavailable;
        }
        crate::broker::claim_lease(lease_id)
            .await
            .unwrap_or(LeaseClaim::Unavailable)
    }

    async fn push(&self) {
        if let Some(uid) = &self.uid
            && let Err(e) = crate::broker::push_dirty(uid, Duration::from_secs(10)).await
        {
            crate::broker::try_log_event(
                "warn",
                "oauth",
                "push after refresh failed; the next sync push retries it",
                Some(&format!("{e:#}")),
            );
        }
    }
}

/// How long a refresh waits on another machine's lease, and how often it
/// looks for that machine's token meanwhile.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timing {
    pub poll: Duration,
    pub max_wait: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(2),
            // Two lease slots and a margin: a holder that crashed is passed
            // over once its lease runs out.
            max_wait: Duration::from_secs(crate::broker::LEASE_SLOT_SECS * 2 + 10),
        }
    }
}

/// Whether `creds` can be used as they are: not about to expire, and not the
/// access token the provider just turned down.
fn usable(creds: &OAuthCredentials, rejected: Option<&str>) -> bool {
    !creds.is_expired() && rejected != Some(creds.access_token.as_str())
}

fn load(kv_key: &str) -> Result<OAuthCredentials> {
    load_credentials(kv_key)?.ok_or_else(|| anyhow!("no stored credentials for '{kv_key}'"))
}

/// A valid credential for `kv_key`, refreshed only if needed, and then by one
/// machine at a time. `rejected` is an access token the provider turned down
/// (a 401), which must be replaced even if it hasn't expired.
pub(crate) async fn refresh_shared<S, R, F>(
    sync: &S,
    kv_key: &str,
    rejected: Option<&str>,
    timing: Timing,
    refresh: R,
) -> Result<OAuthCredentials>
where
    S: RefreshSync,
    R: Fn(OAuthCredentials) -> F,
    F: Future<Output = Result<OAuthCredentials>>,
{
    let _local = LocalLock::acquire(kv_key).await?;

    // Another process here, or another machine, may have refreshed already.
    if let Some(creds) = load_credentials(kv_key)?
        && usable(&creds, rejected)
    {
        return Ok(creds);
    }
    sync.pull().await;
    let mut creds = load(kv_key)?;
    if usable(&creds, rejected) {
        return Ok(creds);
    }

    // One machine refreshes; the rest wait for its token.
    let lease_id = kv_key.to_string();
    let deadline = Instant::now() + timing.max_wait;
    loop {
        match sync.claim(&lease_id).await {
            LeaseClaim::Held { .. } | LeaseClaim::Unavailable => break,
            LeaseClaim::HeldElsewhere { .. } => {
                if Instant::now() >= deadline {
                    crate::broker::try_log_event(
                        "warn",
                        "oauth",
                        "another machine held the refresh lease without refreshing; refreshing here",
                        Some(kv_key),
                    );
                    break;
                }
                tokio::time::sleep(timing.poll).await;
                sync.pull().await;
                creds = load(kv_key)?;
                if usable(&creds, rejected) {
                    return Ok(creds);
                }
            }
        }
    }

    if creds.refresh_token.is_empty() {
        anyhow::bail!("credential '{kv_key}' has no refresh token");
    }
    let first_error = match refresh(creds.clone()).await {
        Ok(new_creds) => return keep(sync, kv_key, new_creds).await,
        Err(e) => e,
    };

    // Spent, most likely because another machine refreshed with the same
    // token. Its new one may have arrived since.
    sync.pull().await;
    let latest = load(kv_key)?;
    if latest.refresh_token == creds.refresh_token || latest.refresh_token.is_empty() {
        return Err(first_error);
    }
    if usable(&latest, rejected) {
        return Ok(latest);
    }
    match refresh(latest).await {
        Ok(new_creds) => keep(sync, kv_key, new_creds).await,
        Err(e) => Err(e.context(format!("after an earlier refresh failed: {first_error:#}"))),
    }
}

async fn keep<S: RefreshSync>(
    sync: &S,
    kv_key: &str,
    creds: OAuthCredentials,
) -> Result<OAuthCredentials> {
    save_credentials(kv_key, &creds)?;
    sync.push().await;
    Ok(creds)
}

/// An exclusive lock on one credential among this machine's processes: a
/// file under `~/.sidekar/locks`, held until dropped.
struct LocalLock {
    _file: std::fs::File,
}

impl LocalLock {
    async fn acquire(kv_key: &str) -> Result<Self> {
        use fs2::FileExt;
        let dir = crate::broker::db_path()
            .parent()
            .map(|p| p.join("locks"))
            .context("no sidekar data directory")?;
        std::fs::create_dir_all(&dir).context("create the credential lock directory")?;
        let name: String = kv_key
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        let path = dir.join(format!("refresh-{name}.lock"));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        // A refresh is a few seconds of network; a process that holds the
        // lock longer than this is stuck, and the refresh goes ahead anyway.
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => {
                    crate::broker::try_log_event(
                        "warn",
                        "oauth",
                        "credential refresh lock held too long; refreshing without it",
                        Some(&format!("{}: {e}", path.display())),
                    );
                    return Ok(Self { _file: file });
                }
            }
        }
    }
}

impl Drop for LocalLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self._file);
    }
}

#[cfg(test)]
mod tests;
