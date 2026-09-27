//! Agent sessions hosted in the background and driven over a socket.
//!
//! The PTY wrapper runs an agent's interactive screen and talks to it by
//! typing: idle detection decides when, a paste delivers the words, and a
//! footer asks the agent to answer with a shell command. Each step guesses, and
//! each guess failed at some point — answers pasted a turn late, an agent that
//! never ran the reply command, a model error that only showed on a screen
//! nobody was watching.
//!
//! A hosted session talks to the agent in the agent's own structured protocol
//! instead (for Claude, `--input-format stream-json`), and serves normalized
//! events on a unix socket. A turn is a message in and a result out; approvals
//! are requests with ids; errors are fields. Nothing is typed and nothing is
//! read off a screen.
//!
//! One host process per session. It owns the engine's pipes, so a crash takes
//! down one session, and a sidekar update does not stop sessions already
//! running. Layout, all under `~/.sidekar/s/<name>/`:
//!
//! - `meta.json` — who and what, written by the host as state changes
//! - `sock`      — the control socket, owner-only
//! - `events.jsonl` — every event, so a client arriving late can catch up
//! - `host.log`  — the host's and the engine's stderr

pub(crate) mod claude;
pub mod cli;
pub(crate) mod engine;
pub(crate) mod host;
pub(crate) mod protocol;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How a session answers a tool approval when the engine asks for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalPolicy {
    /// Surface it and wait for `session approve`.
    Ask,
    Allow,
    Deny,
}

impl ApprovalPolicy {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "ask" => Self::Ask,
            "allow" => Self::Allow,
            "deny" => Self::Deny,
            other => anyhow::bail!("--approvals must be ask, allow or deny, not {other:?}"),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Starting,
    Running,
    Ended,
}

/// What a session is, as the host last recorded it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub name: String,
    pub engine: String,
    pub cwd: String,
    #[serde(default)]
    pub model: Option<String>,
    pub approvals: ApprovalPolicy,
    pub status: Status,
    /// The host process. Zero until the host has started.
    #[serde(default)]
    pub pid: i32,
    /// The engine process the host runs. Recorded because the engine does
    /// not die with its host: Claude in stream-json mode ignores the end of
    /// its input, so a host that is killed leaves it running with nobody to
    /// talk to. Reaping kills it.
    #[serde(default)]
    pub engine_pid: i32,
    /// The engine's own id for the conversation, for resuming it.
    #[serde(default)]
    pub engine_session_id: Option<String>,
    pub created_at: u64,
    #[serde(default)]
    pub ended_at: Option<u64>,
    #[serde(default)]
    pub exit_code: Option<i32>,
}

/// Where sessions live. `SIDEKAR_SESSIONS_DIR` overrides it, for tests: a unix
/// socket path is capped near 104 bytes, and a scratch HOME under the temp
/// directory is already most of that.
pub(crate) fn root() -> PathBuf {
    if let Some(dir) = std::env::var_os("SIDEKAR_SESSIONS_DIR") {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".sidekar")
        .join("s")
}

pub(crate) fn dir_of(name: &str) -> PathBuf {
    root().join(name)
}

pub(crate) fn socket_path(name: &str) -> PathBuf {
    dir_of(name).join("sock")
}

pub(crate) fn events_path(name: &str) -> PathBuf {
    dir_of(name).join("events.jsonl")
}

pub(crate) fn log_path(name: &str) -> PathBuf {
    dir_of(name).join("host.log")
}

fn meta_path(name: &str) -> PathBuf {
    dir_of(name).join("meta.json")
}

pub(crate) fn read_meta(name: &str) -> Result<Meta> {
    let path = meta_path(name);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("no session named {name:?} (`sidekar session list`)"))?;
    serde_json::from_str(&raw).with_context(|| format!("unreadable {}", path.display()))
}

/// Write via a temp file and rename, so a reader never sees half a file.
pub(crate) fn write_meta(meta: &Meta) -> Result<()> {
    let path = meta_path(&meta.name);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(meta)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Every session with a readable meta file, newest first.
pub(crate) fn list() -> Vec<Meta> {
    let Ok(entries) = std::fs::read_dir(root()) else {
        return Vec::new();
    };
    let mut out: Vec<Meta> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| read_meta(&e.file_name().to_string_lossy()).ok())
        .collect();
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    out
}

/// True when the session's host process is still there.
pub(crate) fn host_alive(meta: &Meta) -> bool {
    meta.status != Status::Ended && crate::bus::presence::process_alive(meta.pid)
}

/// A session whose host is gone: its engine stopped, its bus registration
/// settled, and — if it still says running — marked ended with its socket
/// removed. The host does all this itself on a clean exit; this is for the
/// host that crashed or was killed. True when it changed the status.
pub(crate) fn reap(meta: &mut Meta) -> Result<bool> {
    if meta.pid == 0 || crate::bus::presence::process_alive(meta.pid) {
        return Ok(false);
    }
    // Idempotent, and run even for a session already marked ended: what a
    // dead host leaves behind — its engine, its bus registration — is
    // cleaned up whenever it is found, not only the first time.
    kill_orphaned_engine(meta);
    leave_bus(meta);
    if meta.status == Status::Ended {
        return Ok(false);
    }
    meta.status = Status::Ended;
    meta.ended_at.get_or_insert_with(crate::message::epoch_secs);
    write_meta(meta)?;
    let _ = std::fs::remove_file(socket_path(&meta.name));
    Ok(true)
}

/// Take a dead host's session off the bus, settling its mail first so none
/// of it reaches the next agent to use the name. Only the registration this
/// host made: the name may already belong to a new session.
fn leave_bus(meta: &Meta) {
    let pane = format!("session-{}", meta.pid);
    let Ok(Some(agent)) = crate::broker::agent_for_pane_unique(&pane) else {
        return;
    };
    let now = crate::message::epoch_secs();
    let _ = crate::broker::bounce_mail_for_departed(&agent.id.name, &meta.name, now);
    let _ = crate::broker::unregister_agent(&agent.id.name);
}

/// Stop an engine its host left behind — but only if that pid is still the
/// engine: pids are reused, and killing whatever holds it now would be worse
/// than leaving an orphan.
fn kill_orphaned_engine(meta: &Meta) {
    if meta.engine_pid <= 0 || !crate::bus::presence::process_alive(meta.engine_pid) {
        return;
    }
    let command = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &meta.engine_pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    if !is_engine_command(&command, &meta.engine) {
        return;
    }
    unsafe { libc::kill(meta.engine_pid, libc::SIGTERM) };
    // Wait for it: `resume` starts a new engine on the same conversation
    // right after this, and two at once would both write to it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while crate::bus::presence::process_alive(meta.engine_pid) {
        if std::time::Instant::now() >= deadline {
            unsafe { libc::kill(meta.engine_pid, libc::SIGKILL) };
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Whether a process's command line is a session engine of this kind.
pub(crate) fn is_engine_command(command: &str, engine: &str) -> bool {
    let program = command.split_whitespace().next().unwrap_or_default();
    let base = program.rsplit('/').next().unwrap_or_default();
    base == engine && command.contains("stream-json")
}

/// How long an ended session's directory — its event log, its host log —
/// is kept before the daemon's sweep deletes it.
pub(crate) const RETENTION_SECS: u64 = 7 * 24 * 3600;

/// Reap every dead host, and delete sessions that ended more than
/// [`RETENTION_SECS`] ago. Run by the daemon's sweep.
pub fn reap_all() -> usize {
    let now = crate::message::epoch_secs();
    let mut reaped = 0;
    for mut meta in list() {
        if reap(&mut meta).unwrap_or(false) {
            reaped += 1;
        }
        if meta.status == Status::Ended
            && meta
                .ended_at
                .is_some_and(|t| now.saturating_sub(t) > RETENTION_SECS)
        {
            let _ = std::fs::remove_dir_all(dir_of(&meta.name));
        }
    }
    reaped
}

/// `<engine>-<n>` with the lowest `n` no live session holds. An ended
/// session's directory is reused: its name is free and its files are history.
pub(crate) fn free_name(engine: &str) -> String {
    let mut n = 1u32;
    loop {
        let candidate = format!("{engine}-{n}");
        match read_meta(&candidate) {
            Ok(m) if m.status != Status::Ended && (m.pid == 0 || host_alive(&m)) => n += 1,
            _ => return candidate,
        }
    }
}

pub(crate) fn ensure_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(test)]
mod tests;
