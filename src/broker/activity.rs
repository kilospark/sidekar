use super::*;
use crate::activity::{ActivitySnapshot, ActivityState};

pub fn update_agent_activity(name: &str, state: ActivityState, at: u64) -> Result<()> {
    with_cached_conn(|conn| {
        conn.execute(
            "UPDATE agents SET activity_state = ?2, activity_at = ?3 WHERE name = ?1",
            params![name, state.as_str(), at as i64],
        )?;
        Ok(())
    })
}

pub fn get_agent_activity(name: &str) -> Result<Option<ActivitySnapshot>> {
    with_cached_conn(|conn| {
        conn.query_row(
            "SELECT activity_state, activity_at FROM agents WHERE name = ?1",
            params![name],
            |row| {
                let state = ActivityState::parse(&row.get::<_, String>(0)?);
                let at = row.get::<_, i64>(1)? as u64;
                Ok(ActivitySnapshot { state, at })
            },
        )
        .optional()
        .map_err(Into::into)
    })
}
