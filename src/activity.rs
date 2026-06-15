//! Agent activity state for nudge gating and delivery deferral.

use crate::message::epoch_secs;
use std::sync::Mutex;

pub const ACTIVITY_STALE_SECS: u64 = 60;
pub const PTY_OUTPUT_BUSY_MS: u64 = 3_000;
pub const PTY_SPINNER_BUSY_MS: u64 = 10_000;
const USER_TYPING_REFRESH_SECS: u64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityState {
    Unknown,
    Idle,
    UserTyping,
    AgentWorking,
}

impl ActivityState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Idle => "idle",
            Self::UserTyping => "user_typing",
            Self::AgentWorking => "agent_working",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "idle" => Self::Idle,
            "user_typing" => Self::UserTyping,
            "agent_working" => Self::AgentWorking,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivitySnapshot {
    pub state: ActivityState,
    pub at: u64,
}

impl ActivitySnapshot {
    pub fn unknown() -> Self {
        Self {
            state: ActivityState::Unknown,
            at: 0,
        }
    }

    pub fn is_stale(self) -> bool {
        if self.at == 0 {
            return true;
        }
        epoch_secs().saturating_sub(self.at) > ACTIVITY_STALE_SECS
    }

    pub fn should_defer_nudge(self) -> bool {
        if self.is_stale() {
            return false;
        }
        matches!(
            self.state,
            ActivityState::UserTyping | ActivityState::AgentWorking
        )
    }

    pub fn should_defer_delivery(self) -> bool {
        self.should_defer_nudge()
    }
}

static LAST_PUBLISHED: std::sync::LazyLock<
    Mutex<std::collections::HashMap<String, (ActivityState, u64)>>,
> = std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// Persist local activity and mirror to relay when a tunnel is registered.
pub fn publish(agent_name: &str, state: ActivityState) {
    let now = epoch_secs();
    let should_write = {
        let mut last = LAST_PUBLISHED.lock().unwrap_or_else(|e| e.into_inner());
        let should = match last.get(agent_name) {
            Some((prev, at)) if *prev == state => {
                state == ActivityState::UserTyping
                    && now.saturating_sub(*at) >= USER_TYPING_REFRESH_SECS
            }
            _ => true,
        };
        if should {
            last.insert(agent_name.to_string(), (state, now));
        }
        should
    };
    if !should_write {
        return;
    }

    let _ = crate::broker::update_agent_activity(agent_name, state, now);
    publish_relay(state, now);
}

fn publish_relay(state: ActivityState, at: u64) {
    if let Some(ref tx) = crate::tunnel::output_tunnel_sender() {
        tx.send_activity(state, at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_unknown_does_not_defer() {
        let snap = ActivitySnapshot::unknown();
        assert!(snap.is_stale());
        assert!(!snap.should_defer_nudge());
    }

    #[test]
    fn fresh_agent_working_defers() {
        let snap = ActivitySnapshot {
            state: ActivityState::AgentWorking,
            at: epoch_secs(),
        };
        assert!(!snap.is_stale());
        assert!(snap.should_defer_nudge());
    }
}
