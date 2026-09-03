use super::*;
use crate::activity::{ActivitySnapshot, ActivityState};

/// Everything the agents table records about one agent's activity.
#[derive(Debug, Clone)]
pub struct ActivityDetail {
    pub state: ActivityState,
    pub at: u64,
    /// Why the wrapper concluded this state, in the wrapper's own words.
    pub reason: Option<String>,
    /// When the agent last stopped working, if it ever has.
    pub settled_at: Option<u64>,
    /// When a human was last evidently present at the agent's terminal.
    pub seen_at: Option<u64>,
}

impl ActivityDetail {
    pub fn snapshot(&self) -> ActivitySnapshot {
        ActivitySnapshot {
            state: self.state,
            at: self.at,
        }
    }

    /// True when the agent finished a turn that nobody has looked at since.
    ///
    /// Herdr splits `idle` from `done` on whether the pane was visible in the
    /// focused UI. Sidekar has no UI to focus, and a visible tab is weak
    /// evidence anyway — it can sit on a second monitor for an hour. The
    /// signal used instead is the human typing into that agent's own terminal,
    /// which is proof of presence rather than a proxy for it.
    pub fn finished_unseen(&self) -> bool {
        match self.settled_at {
            None => false,
            Some(settled) => self.seen_at.is_none_or(|seen| seen < settled),
        }
    }
}

pub fn update_agent_activity(name: &str, state: ActivityState, at: u64) -> Result<()> {
    update_agent_activity_with_reason(name, state, at, None)
}

/// Record activity, maintaining the finish and presence marks alongside it.
///
/// Both marks are derived from the transition rather than reported separately,
/// so a wrapper that only knows its current state cannot get them out of sync
/// with it.
pub fn update_agent_activity_with_reason(
    name: &str,
    state: ActivityState,
    at: u64,
    reason: Option<&str>,
) -> Result<()> {
    with_cached_conn(|conn| {
        let previous: Option<String> = conn
            .query_row(
                "SELECT activity_state FROM agents WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .optional()?;
        let was_working =
            previous.as_deref().map(ActivityState::parse) == Some(ActivityState::AgentWorking);

        conn.execute(
            "UPDATE agents SET activity_state = ?2, activity_at = ?3, activity_reason = ?4 \
             WHERE name = ?1",
            params![name, state.as_str(), at as i64, reason],
        )?;

        // A turn just ended. Mark the finish so it can be reported as unseen.
        if was_working && matches!(state, ActivityState::Idle | ActivityState::NeedsInput) {
            conn.execute(
                "UPDATE agents SET settled_at = ?2 WHERE name = ?1",
                params![name, at as i64],
            )?;
        }
        // The human is typing into this agent's terminal, so they are there.
        if state == ActivityState::UserTyping {
            conn.execute(
                "UPDATE agents SET seen_at = ?2 WHERE name = ?1",
                params![name, at as i64],
            )?;
        }
        Ok(())
    })
}

/// Mark an agent's finish as seen without waiting for the human to type.
pub fn mark_agent_seen(name: &str, at: u64) -> Result<()> {
    with_cached_conn(|conn| {
        conn.execute(
            "UPDATE agents SET seen_at = ?2 WHERE name = ?1",
            params![name, at as i64],
        )?;
        Ok(())
    })
}

pub fn get_agent_activity(name: &str) -> Result<Option<ActivitySnapshot>> {
    Ok(get_agent_activity_detail(name)?.map(|d| d.snapshot()))
}

pub fn get_agent_activity_detail(name: &str) -> Result<Option<ActivityDetail>> {
    with_cached_conn(|conn| {
        conn.query_row(
            "SELECT activity_state, activity_at, activity_reason, settled_at, seen_at \
             FROM agents WHERE name = ?1",
            params![name],
            |row| {
                Ok(ActivityDetail {
                    state: ActivityState::parse(&row.get::<_, String>(0)?),
                    at: row.get::<_, i64>(1)? as u64,
                    reason: row.get::<_, Option<String>>(2)?,
                    settled_at: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                    seen_at: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                })
            },
        )
        .optional()
    })
}
