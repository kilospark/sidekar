//! Codex ChatGPT footer: merges cached plan quota (`wham/usage`) into REPL usage line.

use crate::providers::RateLimitSnapshot;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(super) struct CodexFooterBindings {
    pub(super) cache: Arc<Mutex<CodexWhamFooterCache>>,
    pub(super) api_key: String,
    pub(super) account_id: String,
}

#[derive(Debug, Clone, Default)]
pub(super) struct CodexWhamFooterCache {
    /// Last merged quota snapshot (headers + stream + optional wham).
    pub(super) snapshot: Option<RateLimitSnapshot>,
    last_wham_attempt: Option<Instant>,
}

impl CodexWhamFooterCache {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Spawn `wham/usage` at most once per cooldown when streamed quota line empty.
    pub(super) fn should_spawn_wham(&mut self, merged_shows_quota: bool) -> bool {
        const COOLDOWN: Duration = Duration::from_secs(600);
        if merged_shows_quota {
            return false;
        }
        match self.last_wham_attempt {
            None => true,
            Some(t) => t.elapsed() >= COOLDOWN,
        }
    }

    pub(super) fn note_wham_spawned(&mut self) {
        self.last_wham_attempt = Some(Instant::now());
    }

    /// Drop merged quota + wham throttle so next `/credential` cannot inherit prior account.
    pub(super) fn clear(&mut self) {
        self.snapshot = None;
        self.last_wham_attempt = None;
    }
}

pub(super) fn spawn_wham_quota_refresh(
    bindings: CodexFooterBindings,
    main_line_already_had_quota: bool,
) {
    tokio::spawn(async move {
        let Ok(v) = crate::providers::codex::fetch_codex_plan_quota_json(
            bindings.api_key.as_str(),
            bindings.account_id.as_str(),
        )
        .await
        else {
            return;
        };
        let Some(fresh) = crate::providers::codex::rate_limit_snapshot_from_codex_quota_json(&v)
        else {
            return;
        };
        let updated = {
            let Ok(mut g) = bindings.cache.lock() else {
                return;
            };
            g.snapshot = RateLimitSnapshot::overlay_option(g.snapshot.clone(), Some(fresh));
            g.snapshot.clone()
        };
        if main_line_already_had_quota {
            return;
        }
        if let Some(ref snap) = updated
            && let Some(line) = super::ratelimit::format_plan_quota_fallback_line(snap)
        {
            super::editor::emit_shared_line(&line);
        }
    });
}
