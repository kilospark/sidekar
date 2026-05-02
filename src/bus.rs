//! Sidekar bus — registration, message tracking, and coordination.
//!
//! Provides agent identity management, pending message tracking, nudge
//! timers, and bus handoffs. Durable state stored in the broker
//! SQLite database, delivery via the broker message queue.

use crate::broker::{self, BrokerAgent};
use crate::message::{AgentId, Envelope, MessageKind, epoch_secs};
use crate::transport::Transport;
use crate::*;
use std::io::Write as _;

mod commands;
mod nickname;

pub use commands::*;
pub use nickname::*;

const PENDING_GRACE_SECS: u64 = 30;
const TIMEOUT_SECS: u64 = 300;
const BROKER_TRANSPORT: &str = "broker";
const RELAY_HTTP_TRANSPORT: &str = "relay_http";
const DEFAULT_BUS_LIST_LIMIT: usize = 20;

// --- Terminal title helper ---

/// Write an OSC 0 escape sequence to set the terminal title.
/// Tries `/dev/tty` first (works even when stderr is redirected, e.g. in PTY
/// mode), then falls back to stderr.
pub fn set_terminal_title(title: &str) {
    let seq = format!("\x1b]0;{title}\x07");
    if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
        let _ = tty.write_all(seq.as_bytes());
        let _ = tty.flush();
    } else {
        eprint!("{seq}");
    }
}

// --- PTY registration inheritance ---

/// Check if a parent process registered a PTY session in the broker.
/// Walks the process tree looking for a parent PID that matches a
/// `pty-<pid>` entry in the broker's agents table.
pub fn inherit_pty_registration() -> Option<AgentId> {
    let mut pid = std::process::id();
    loop {
        if pid <= 1 {
            break;
        }
        // Walk to parent
        pid = match Command::new("ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse::<u32>()
                    .ok()
            }) {
            Some(ppid) if ppid != pid && ppid > 1 => ppid,
            _ => break,
        };
        // Check if this ancestor registered a PTY session
        let pty_id = format!("pty-{pid}");
        if let Ok(Some(agent)) = broker::agent_for_pane_unique(&pty_id) {
            return Some(agent.id);
        }
    }
    None
}

/// Resolve the broker [`crate::broker::AgentId::name`] row used as `bus_queue.recipient`
/// for subprocesses (CDP monitor, `sidekar ext watch`) that are not the registering
/// process themselves.
///
/// Order: runtime / `SIDEKAR_AGENT_NAME`, PTY parent-chain
/// ([`inherit_pty_registration`]), then `pty-` / `repl-` / `cli-` + current PID.
/// Does not fall back to an arbitrary registered agent (unlike CDP monitor startup).
pub fn resolve_registered_agent_bus_name_for_current_process() -> Option<String> {
    let name_hint = crate::runtime::agent_name().or_else(|| {
        std::env::var("SIDEKAR_AGENT_NAME")
            .ok()
            .filter(|s| !s.trim().is_empty())
    });

    if let Some(ref nm) = name_hint
        && let Ok(Some(agent)) = broker::find_agent(nm, None)
    {
        return Some(agent.id.name);
    }

    if let Some(agent_id) = inherit_pty_registration() {
        return Some(agent_id.name);
    }

    let my_pid = std::process::id();
    for prefix in ["pty-", "repl-", "cli-"] {
        let pane = format!("{prefix}{my_pid}");
        if let Ok(Some(a)) = broker::agent_for_pane_unique(&pane) {
            return Some(a.id.name);
        }
    }

    None
}

// --- Agent type detection ---

/// Walk process tree to detect agent type (claude, codex, copilot, etc.).
fn detect_agent_type() -> String {
    let mut pid = std::process::id();
    loop {
        if pid <= 1 {
            break;
        }
        if let Ok(output) = Command::new("ps")
            .args(["-o", "comm=", "-p", &pid.to_string()])
            .output()
        {
            let comm = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let name = comm.rsplit('/').next().unwrap_or("").to_lowercase();
            if name == "claude" || name.starts_with("claude-") {
                return "claude".into();
            }
            if name.starts_with("codex") {
                return "codex".into();
            }
            if name.starts_with("copilot") {
                return "copilot".into();
            }
            if name == "agent" {
                return "agent".into();
            }
            if name == "gemini" || name.starts_with("gemini-") {
                return "gemini".into();
            }
            if name == "opencode" || name.starts_with("opencode-") {
                return "opencode".into();
            }
            if name == "node"
                && let Ok(args_out) = Command::new("ps")
                    .args(["-o", "args=", "-p", &pid.to_string()])
                    .output()
            {
                let args = String::from_utf8_lossy(&args_out.stdout).to_lowercase();
                for known in ["gemini", "opencode", "copilot"] {
                    if args.contains(known) {
                        return known.into();
                    }
                }
            }
        }
        match Command::new("ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse::<u32>()
                    .ok()
            }) {
            Some(ppid) if ppid != pid => pid = ppid,
            _ => break,
        }
    }
    "unknown".into()
}

/// Detect the project name from the pane's working directory.
/// Uses the full path for consistency with KV store.
pub fn detect_project_name() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

#[derive(Debug, Clone, Default)]
pub struct SidekarBusState {
    /// Agent identity (None if not registered).
    pub identity: Option<AgentId>,
    /// Unique pane/session ID (e.g. "pty-12345" or "mcp-12345").
    pub pane_unique_id: Option<String>,
    /// True when identity was inherited from a parent PTY wrapper.
    pub inherited_pty: bool,
    /// True when identity was borrowed from another process (CLI recovering PTY state).
    /// Drop will NOT unregister — the owning process manages the registration.
    pub borrowed: bool,
}

impl SidekarBusState {
    pub fn new() -> Self {
        Self {
            identity: None,
            pane_unique_id: None,
            inherited_pty: false,
            borrowed: false,
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.identity.as_ref().map(|id| id.name.as_str())
    }

    pub fn nick(&self) -> Option<&str> {
        self.identity.as_ref().and_then(|id| id.nick.as_deref())
    }

    pub fn pane(&self) -> Option<&str> {
        self.identity.as_ref().and_then(|id| id.pane.as_deref())
    }

    pub fn channel(&self) -> Option<&str> {
        self.identity.as_ref().and_then(|id| id.session.as_deref())
    }

    pub fn agent_id(&self) -> AgentId {
        self.identity
            .clone()
            .unwrap_or_else(|| AgentId::new("unknown"))
    }

    pub fn touch(&self) {
        if let Some(name) = self.name() {
            let _ = broker::touch_agent(name);
        }
    }

    pub fn unregister(&mut self) {
        if let Some(name) = self.name().map(String::from) {
            let _ = broker::unregister_agent(&name);
        }

        self.identity = None;
        self.pane_unique_id = None;
    }

    pub fn do_register(&mut self, custom_name: Option<&str>) {
        if self.identity.is_some() {
            self.unregister();
        }

        // Check if a parent PTY wrapper already registered us
        if let Some(inherited) = inherit_pty_registration() {
            // inherited PTY registration
            let nick = inherited.nick.as_deref().unwrap_or("?");
            let name = &inherited.name;
            let agent_type = detect_agent_type();
            set_terminal_title(&format!("{nick} ({name}) - {agent_type}"));
            self.pane_unique_id = inherited.pane.clone();
            self.inherited_pty = true;
            self.identity = Some(inherited);
            return;
        }

        // Not in a PTY wrapper — register as a standalone session
        let channel = crate::pty::detect_channel();
        let pane_unique = format!("cli-{}", std::process::id());
        let project = detect_project_name();

        let name = if let Some(custom) = custom_name {
            custom.to_string()
        } else {
            let agent_type = detect_agent_type();
            let existing_names: HashSet<String> = broker::list_agents(None)
                .unwrap_or_default()
                .into_iter()
                .map(|a| a.id.name)
                .collect();
            let mut n = 1u32;
            loop {
                let candidate = format!("{agent_type}-{project}-{n}");
                if !existing_names.contains(&candidate) {
                    break candidate;
                }
                n += 1;
            }
        };

        let nick = pick_nickname_for_project(Some(&project));

        let identity = AgentId {
            name,
            nick: Some(nick),
            session: Some(channel),
            pane: Some(pane_unique.clone()),
            agent_type: Some("sidekar".into()),
        };

        if let Err(e) = broker::register_agent(&identity, Some(&pane_unique)) {
            let _ = e;
        }

        self.identity = Some(identity);
        self.pane_unique_id = Some(pane_unique);

        if let (Some(name), Some(nick), Some(_channel)) = (self.name(), self.nick(), self.channel())
        {
            let agent_type = detect_agent_type();
            set_terminal_title(&format!("{nick} ({name}) - {agent_type}"));
        }
    }
}

impl Drop for SidekarBusState {
    fn drop(&mut self) {
        if !self.borrowed {
            self.unregister();
        }
    }
}

fn pending_message_exists(msg_id: &str) -> bool {
    matches!(broker::pending_message(msg_id), Ok(Some(_)))
}

/// Build a warning string for unanswered pending messages and queued bus messages.
pub fn pending_warnings(state: &SidekarBusState) -> Option<String> {
    let name = state.name()?;
    let mut warnings = Vec::new();

    // Check envelope-based pending messages
    if let Ok(pending) = broker::pending_for_agent(name) {
        let now = epoch_secs();
        for env in &pending {
            if env.created_at > 0 && now.saturating_sub(env.created_at) < PENDING_GRACE_SECS {
                continue;
            }
            let kind = env.kind.as_str();
            warnings.push(format!(
                "  [{kind} id={} from {}]: {}",
                env.id,
                env.from.display_name(),
                env.preview()
            ));
        }
    }

    // Drain bus_queue messages (from broker transport / cron / monitor)
    if let Ok(queued) = broker::poll_messages(name) {
        for msg in &queued {
            warnings.push(format!("  {}", msg.body));
        }
    }

    if warnings.is_empty() {
        return None;
    }
    Some(format!(
        "\u{26a0} You have {} unanswered message(s). Respond using bus done or bus send with --reply-to=<msg_id>:\n{}",
        warnings.len(),
        warnings.join("\n")
    ))
}

pub fn check_outbound_timeouts(state: &SidekarBusState) -> Option<String> {
    let name = state.name()?;
    let cutoff = epoch_secs().saturating_sub(TIMEOUT_SECS);
    let expired = broker::expired_outbound_for_sender(name, cutoff).ok()?;
    if expired.is_empty() {
        return None;
    }

    let mut warnings = Vec::new();
    for request in expired {
        if pending_message_exists(&request.msg_id) {
            let _ = broker::clear_pending(&request.msg_id);
            let _ = broker::mark_outbound_timed_out(&request.msg_id, epoch_secs());
            warnings.push(format!(
                "No response from {} to request {} after {}s.",
                request.recipient_name, request.msg_id, TIMEOUT_SECS
            ));
        }
    }

    if warnings.is_empty() {
        None
    } else {
        Some(warnings.join("\n"))
    }
}
