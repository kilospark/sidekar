//! One agent's presence on the bus, from registration to departure.
//!
//! Before this existed, every way of running an agent built its own identity,
//! registered it, and remembered — or didn't — to leave. PTY, REPL and the CLI
//! each carried a copy of the same naming loop, three different policies for a
//! failed registration, and their own list of exit paths to unregister on. The
//! copies drifted: REPL registered and then returned early on a missing model
//! without unregistering, leaving a ghost in `bus who` until the reaper found it.
//!
//! [`Presence`] is the part every long-lived agent session has in common:
//! a name nobody else holds, an identity, a registration, an optional
//! session-history row, and a departure that happens exactly once however the
//! session ends. Things that differ between modes — activity reporting, the
//! relay, journaling, bus delivery into a terminal — stay with the modes that
//! own them.

use crate::broker;
use crate::message::AgentId;
use anyhow::Result;
use std::collections::HashSet;

/// A name nobody on the bus holds: `{prefix}-{n}` with the lowest free `n`.
///
/// Racy by nature — two agents starting in the same instant can both see `n` as
/// free. That was true of all three copies this replaces, and the registry's
/// upsert means the loser overwrites rather than fails, so this does not make it
/// worse; it only stops the three from disagreeing about the format.
pub fn unique_name(prefix: &str) -> String {
    let taken: HashSet<String> = broker::list_agents(None)
        .unwrap_or_default()
        .into_iter()
        .map(|a| a.id.name)
        .collect();
    first_free(prefix, &taken)
}

/// The naming rule on its own, so it can be tested without a broker.
pub(crate) fn first_free(prefix: &str, taken: &HashSet<String>) -> String {
    let mut n = 1u32;
    loop {
        let candidate = format!("{prefix}-{n}");
        if !taken.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// What to register an agent as.
pub struct Registration {
    pub name: String,
    pub nick: String,
    /// The bus channel. Agents in the same directory share one, which is how
    /// `bus who` groups them.
    pub channel: String,
    /// Unique per process: `pty-<pid>`, `repl-<pid>`.
    pub pane: String,
    /// What kind of process this is, as recorded on the bus.
    pub agent_type: &'static str,
    /// A row in session history, for modes that keep one.
    pub history: Option<History>,
}

/// The session-history row an agent writes on arrival and closes on departure.
///
/// Optional because only PTY sessions record one today. That is a real gap —
/// `sidekar agent-sessions` shows no REPL sessions — but closing it is a
/// behaviour change, and this module's first job was to change none.
pub struct History {
    pub id: String,
    /// The harness being run, e.g. `claude`.
    pub agent: String,
    pub cwd: String,
    pub started_at: u64,
}

/// An agent registered on the bus.
///
/// Leaving happens once: on an explicit [`Presence::leave`], or on drop if
/// nothing called it. The drop is what fixes early returns — a `?` or `bail!`
/// between registering and the end of a session no longer strands a ghost
/// registration.
///
/// `std::process::exit` does not run destructors, so a session that ends that
/// way must call `leave` itself first. The PTY wrapper does.
pub struct Presence {
    identity: AgentId,
    history_id: Option<String>,
    left: bool,
}

impl Presence {
    /// Register, and open a history row if the registration asks for one.
    ///
    /// All or nothing: if the history row cannot be written the registration is
    /// undone before returning, so a failure here never leaves half an agent.
    pub fn register(r: Registration) -> Result<Self> {
        let identity = AgentId {
            name: r.name,
            nick: Some(r.nick),
            session: Some(r.channel),
            pane: Some(r.pane.clone()),
            agent_type: Some(r.agent_type.to_string()),
        };
        broker::register_agent(&identity, Some(&r.pane))?;

        let history_id = match r.history {
            None => None,
            Some(h) => {
                if let Err(e) = broker::create_agent_session(
                    &h.id,
                    &identity.name,
                    Some(&h.agent),
                    identity.nick.as_deref(),
                    &h.cwd,
                    identity.session.as_deref(),
                    Some(&h.cwd),
                    h.started_at,
                ) {
                    let _ = broker::unregister_agent(&identity.name);
                    return Err(e);
                }
                Some(h.id)
            }
        };

        Ok(Self {
            identity,
            history_id,
            left: false,
        })
    }

    pub fn name(&self) -> &str {
        &self.identity.name
    }

    /// Close the history row, then leave the bus. Safe to call more than once.
    ///
    /// History closes first so the row's `ended_at` is never later than the
    /// moment the agent stopped being reachable — the order PTY always used.
    pub fn leave(&mut self) {
        if self.left {
            return;
        }
        self.left = true;
        if let Some(id) = &self.history_id {
            let _ = broker::finish_agent_session(id, crate::message::epoch_secs());
        }
        let _ = broker::unregister_agent(&self.identity.name);
    }
}

impl Drop for Presence {
    fn drop(&mut self) {
        self.leave();
    }
}

#[cfg(test)]
mod tests;
