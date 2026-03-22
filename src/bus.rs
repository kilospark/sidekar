//! Sidekar bus — registration, message tracking, and coordination.
//!
//! Provides agent identity management, pending message tracking, nudge
//! timers, broadcast, and bus handoffs. Built on top of the ipc module's
//! socket and paste primitives, with durable state stored in the broker.

use crate::broker::{self, BrokerAgent};
use crate::ipc;
use crate::message::{AgentId, Envelope, MessageKind, epoch_secs};
use crate::transport::{Socket, TmuxPaste, Transport};
use crate::*;

const PENDING_GRACE_SECS: u64 = 30;
const TIMEOUT_SECS: u64 = 300;
const NUDGE_INTERVAL_SECS: u64 = 45;
const NUDGE_BACKOFF_SECS: u64 = 90;
const NUDGE_MAX: u32 = 3;
const NUDGE_POLL_SECS: u64 = 5;
const NUDGE_BUSY_CHECK_SECS: u64 = 3;
const TMUX_TRANSPORT: &str = "tmux-paste";
const SOCKET_TRANSPORT: &str = "unix-socket";

// --- PTY registration inheritance ---

/// Check if a parent process registered a PTY session in the broker.
/// Walks the process tree looking for a parent PID that matches a
/// `pty-<pid>` entry in the broker's agents table.
fn inherit_pty_registration() -> Option<AgentId> {
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
            if name == "node" {
                if let Ok(args_out) = Command::new("ps")
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
/// Tries git repo root name first, falls back to the cwd basename.
fn detect_project_name() -> String {
    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if let Some(name) = path.rsplit('/').next() {
                if !name.is_empty() {
                    let clean: String = name
                        .to_lowercase()
                        .chars()
                        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                        .collect();
                    return clean.trim_matches('-').to_string();
                }
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        if let Some(name) = cwd.file_name() {
            let name = name.to_string_lossy().to_lowercase();
            let clean: String = name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            return clean.trim_matches('-').to_string();
        }
    }

    "unknown".into()
}

/// 100 short, memorable nicknames for agents.
const NICKNAMES: &[&str] = &[
    "badger",
    "bantam",
    "barbet",
    "basilisk",
    "bison",
    "bobcat",
    "bonobo",
    "borzoi",
    "caiman",
    "capybara",
    "caracal",
    "cassowary",
    "cheetah",
    "chinchilla",
    "cicada",
    "civet",
    "coati",
    "condor",
    "corgi",
    "cougar",
    "coyote",
    "crane",
    "cuckoo",
    "curlew",
    "dingo",
    "dormouse",
    "drongo",
    "dugong",
    "dunlin",
    "egret",
    "ermine",
    "falcon",
    "fennec",
    "ferret",
    "finch",
    "flamingo",
    "flounder",
    "gannet",
    "gazelle",
    "gecko",
    "gerbil",
    "gibbon",
    "gopher",
    "grouse",
    "guppy",
    "harrier",
    "hedgehog",
    "heron",
    "hoopoe",
    "hornet",
    "husky",
    "hyena",
    "ibis",
    "iguana",
    "impala",
    "jackal",
    "jackdaw",
    "jaguar",
    "jerboa",
    "kakapo",
    "kestrel",
    "kinkajou",
    "kiwi",
    "kodiak",
    "komodo",
    "lemur",
    "leopard",
    "limpet",
    "loris",
    "macaw",
    "mako",
    "mamba",
    "mandrill",
    "mantis",
    "margay",
    "marlin",
    "marmot",
    "merlin",
    "mink",
    "mongoose",
    "moray",
    "narwhal",
    "newt",
    "numbat",
    "ocelot",
    "okapi",
    "oriole",
    "osprey",
    "otter",
    "pangolin",
    "parrot",
    "pelican",
    "penguin",
    "peregrine",
    "pika",
    "piranha",
    "platypus",
    "quail",
    "quetzal",
    "quokka",
    "raven",
    "robin",
    "rooster",
    "sable",
    "salmon",
    "scarab",
    "serval",
    "shrike",
    "sparrow",
    "starling",
    "stoat",
    "taipan",
    "tamarin",
    "tanager",
    "tarpon",
    "tenrec",
    "tern",
    "thrush",
    "toucan",
    "uakari",
    "umbrellabird",
    "viper",
    "vizsla",
    "vulture",
    "wallaby",
    "walrus",
    "weasel",
    "whippet",
    "wombat",
    "woodpecker",
    "xerus",
    "yak",
    "zebu",
    "zorilla",
];

/// Pick a random unused nickname from the pool.
fn pick_nickname() -> String {
    use rand::seq::SliceRandom;

    let used: HashSet<String> = list_tmux_agents()
        .into_iter()
        .filter_map(|a| a.id.nick)
        .collect();
    let mut available: Vec<&str> = NICKNAMES
        .iter()
        .filter(|n| !used.contains(**n))
        .copied()
        .collect();
    available.shuffle(&mut rand::rng());
    available.first().map(|s| s.to_string()).unwrap_or_else(|| {
        let r: u16 = rand::random();
        format!("agent-{:04x}", r)
    })
}

/// Pick a nickname using the broker for used-name checks (works without tmux).
pub fn pick_nickname_standalone() -> String {
    use rand::seq::SliceRandom;

    let mut used: HashSet<String> = HashSet::new();
    // Check broker
    if let Ok(agents) = broker::list_agents(None) {
        for agent in agents {
            if let Some(nick) = agent.id.nick {
                used.insert(nick);
            }
        }
    }
    // Also check tmux if available
    for agent in list_tmux_agents() {
        if let Some(nick) = agent.id.nick {
            used.insert(nick);
        }
    }
    let mut available: Vec<&str> = NICKNAMES
        .iter()
        .filter(|n| !used.contains(**n))
        .copied()
        .collect();
    available.shuffle(&mut rand::rng());
    available.first().map(|s| s.to_string()).unwrap_or_else(|| {
        let r: u16 = rand::random();
        format!("agent-{:04x}", r)
    })
}

#[derive(Debug, Clone)]
struct DeliveryTarget {
    transport_name: &'static str,
    transport_target: String,
    output_label: String,
}

#[derive(Debug, Clone)]
struct TmuxAgent {
    id: AgentId,
    pane_target: String,
}

impl TmuxAgent {
    fn name(&self) -> &str {
        &self.id.name
    }

    fn nick(&self) -> Option<&str> {
        self.id.nick.as_deref()
    }

    fn pane_display(&self) -> &str {
        self.id.pane.as_deref().unwrap_or("?")
    }

    fn pane_target(&self) -> &str {
        &self.pane_target
    }

    fn session(&self) -> &str {
        self.id.session.as_deref().unwrap_or("?")
    }
}

#[derive(Default)]
pub struct SidekarBusState {
    /// Agent identity (None if not registered).
    pub identity: Option<AgentId>,
    /// Unique tmux pane ID, e.g. "%42" (transport-specific).
    pub pane_unique_id: Option<String>,
    pub socket_path: Option<PathBuf>,
    active_nudges: HashSet<String>,
}

impl SidekarBusState {
    pub fn new() -> Self {
        Self {
            identity: None,
            pane_unique_id: None,
            socket_path: None,
            active_nudges: HashSet::new(),
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
            if let Err(e) = broker::touch_agent(name) {
                eprintln!("sidekar bus: failed to heartbeat agent {name}: {e}");
            }
        }
    }

    pub fn set_socket_path(&mut self, socket_path: Option<PathBuf>) {
        self.socket_path = socket_path.clone();
        if let Some(name) = self.name() {
            if let Err(e) = broker::set_agent_socket_path(name, socket_path.as_deref()) {
                eprintln!("sidekar bus: failed to update socket path for {name}: {e}");
            }
        }
    }

    fn ensure_nudge_timer(&mut self, msg_id: &str) {
        if self.active_nudges.insert(msg_id.to_string()) {
            spawn_nudge_timer(msg_id.to_string());
        }
    }

    fn resume_nudges(&mut self) {
        let Some(name) = self.name().map(String::from) else {
            return;
        };
        match broker::outbound_for_sender(&name) {
            Ok(requests) => {
                for request in requests {
                    self.ensure_nudge_timer(&request.msg_id);
                }
            }
            Err(e) => eprintln!("sidekar bus: failed to restore outbound state for {name}: {e}"),
        }
    }

    pub fn unregister(&mut self) {
        if let Some(name) = self.name().map(String::from) {
            if let Err(e) = broker::unregister_agent(&name) {
                eprintln!("sidekar bus: broker unregister failed for {name}: {e}");
            } else {
                eprintln!("sidekar bus: unregistered \"{name}\"");
            }
        }

        if let Some(pane) = self.pane().map(String::from) {
            let _ = Command::new("tmux")
                .args(["set-option", "-pu", "-t", &pane, "pane-border-format"])
                .status();
        }

        self.identity = None;
        self.pane_unique_id = None;
        self.active_nudges.clear();
    }

    pub fn do_register(&mut self, custom_name: Option<&str>) {
        if self.identity.is_some() {
            self.unregister();
        }

        let (pane, pane_unique, session) = match ipc::detect_tmux_pane() {
            Some(detected) => (detected.display_id, detected.unique_id, detected.session),
            None => {
                // Not in tmux — check if a parent PTY wrapper registered us
                if let Some(inherited) = inherit_pty_registration() {
                    eprintln!(
                        "sidekar bus: inherited PTY registration as \"{}\" aka \"{}\" on channel \"{}\"",
                        inherited.name,
                        inherited.nick.as_deref().unwrap_or("?"),
                        inherited.session.as_deref().unwrap_or("?"),
                    );
                    self.pane_unique_id = inherited.pane.clone();
                    self.identity = Some(inherited);
                }
                return;
            }
        };

        let name = if let Some(custom) = custom_name {
            custom.to_string()
        } else {
            let agent_type = detect_agent_type();
            let project = detect_project_name();
            let existing_names: HashSet<String> =
                list_tmux_agents().into_iter().map(|a| a.id.name).collect();
            let mut n = 1u32;
            loop {
                let candidate = format!("{agent_type}-{project}-{n}");
                if !existing_names.contains(&candidate) {
                    break candidate;
                }
                n += 1;
            }
        };

        let nick = pick_nickname();

        let _ = Command::new("tmux")
            .args([
                "set-option",
                "-p",
                "-t",
                &pane,
                "pane-border-format",
                &format!(" {nick} ({name}) | #{{pane_title}} "),
            ])
            .status();

        let border_status = Command::new("tmux")
            .args(["show-option", "-pv", "-t", &pane, "pane-border-status"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();
        if border_status.trim().is_empty() || border_status.trim() == "off" {
            let _ = Command::new("tmux")
                .args(["set-option", "-p", "-t", &pane, "pane-border-status", "top"])
                .status();
        }

        let identity = AgentId {
            name,
            nick: Some(nick),
            session: Some(session),
            pane: Some(pane),
            agent_type: Some("sidekar".into()),
        };

        if let Err(e) = broker::register_agent(&identity, Some(&pane_unique)) {
            eprintln!(
                "sidekar bus: broker register failed for {}: {e}",
                identity.name
            );
        }

        self.identity = Some(identity);
        self.pane_unique_id = Some(pane_unique);
        self.active_nudges.clear();

        if let Some(name) = self.name() {
            if let Err(e) = broker::set_agent_socket_path(name, self.socket_path.as_deref()) {
                eprintln!("sidekar bus: failed to persist socket path for {name}: {e}");
            }
        }

        if let (Some(name), Some(nick), Some(channel), Some(pane)) =
            (self.name(), self.nick(), self.channel(), self.pane())
        {
            eprintln!(
                "sidekar bus: registered as \"{name}\" aka \"{nick}\" on channel \"{channel}\" (pane {pane})"
            );
        }

        self.resume_nudges();
    }
}

impl Drop for SidekarBusState {
    fn drop(&mut self) {
        self.unregister();
    }
}

fn current_tmux_panes() -> HashMap<String, String> {
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}\t#{session_name}:#{window_index}.#{pane_index}",
        ])
        .output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return HashMap::new(),
    };
    let mut panes = HashMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut parts = line.split('\t');
        let Some(unique_id) = parts.next() else {
            continue;
        };
        let Some(display_id) = parts.next() else {
            continue;
        };
        panes.insert(unique_id.to_string(), display_id.to_string());
    }
    panes
}

fn broker_agent_to_tmux(
    agent: BrokerAgent,
    pane_map: &HashMap<String, String>,
) -> Option<TmuxAgent> {
    let target = agent
        .pane_unique_id
        .clone()
        .or_else(|| agent.id.pane.clone())?;

    let mut id = agent.id;
    if let Some(unique_id) = agent.pane_unique_id {
        let display_id = pane_map.get(&unique_id)?.clone();
        id.pane = Some(display_id);
        Some(TmuxAgent {
            id,
            pane_target: unique_id,
        })
    } else {
        Some(TmuxAgent {
            id,
            pane_target: target,
        })
    }
}

fn list_tmux_agents() -> Vec<TmuxAgent> {
    let pane_map = current_tmux_panes();
    match broker::list_agents(None) {
        Ok(agents) => agents
            .into_iter()
            .filter_map(|agent| broker_agent_to_tmux(agent, &pane_map))
            .collect(),
        Err(e) => {
            eprintln!("sidekar bus: failed to read broker agent list: {e}");
            Vec::new()
        }
    }
}

fn find_agent_on_channel(name_or_nick: &str, channel: &str) -> Option<TmuxAgent> {
    list_tmux_agents().into_iter().find(|a| {
        (a.name() == name_or_nick || a.nick() == Some(name_or_nick)) && a.session() == channel
    })
}

fn agents_on_channel(session: &str, exclude: &str) -> Vec<TmuxAgent> {
    list_tmux_agents()
        .into_iter()
        .filter(|a| a.session() == session && a.name() != exclude)
        .collect()
}

fn available_agents_str(channel: &str, exclude: &str) -> String {
    let agents = agents_on_channel(channel, exclude);
    if agents.is_empty() {
        "none".to_string()
    } else {
        agents
            .iter()
            .map(|a| a.id.display_name())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn pending_message_exists(msg_id: &str) -> bool {
    matches!(broker::pending_message(msg_id), Ok(Some(_)))
}

fn deliver_via(transport_name: &str, target: &str, message: &str, from: &str) -> Result<()> {
    let result = match transport_name {
        TMUX_TRANSPORT => TmuxPaste.deliver(target, message, from)?,
        SOCKET_TRANSPORT => Socket.deliver(target, message, from)?,
        other => bail!("unknown transport: {other}"),
    };
    match result {
        crate::message::DeliveryResult::Delivered | crate::message::DeliveryResult::Queued => {
            Ok(())
        }
        crate::message::DeliveryResult::Failed(reason) => bail!("delivery failed: {reason}"),
    }
}

fn spawn_nudge_timer(msg_id: String) {
    std::thread::spawn(move || {
        let mut wait_secs = NUDGE_INTERVAL_SECS;

        loop {
            let mut elapsed = 0u64;
            while elapsed < wait_secs {
                std::thread::sleep(Duration::from_secs(NUDGE_POLL_SECS));
                elapsed += NUDGE_POLL_SECS;
                if broker::outbound_request(&msg_id).ok().flatten().is_none() {
                    return;
                }
            }

            let Some(request) = broker::outbound_request(&msg_id).ok().flatten() else {
                return;
            };

            if !pending_message_exists(&msg_id) {
                let _ = broker::delete_outbound_request(&msg_id);
                return;
            }

            if request.transport_name == TMUX_TRANSPORT {
                let snap1 = ipc::capture_pane(&request.transport_target);
                std::thread::sleep(Duration::from_secs(NUDGE_BUSY_CHECK_SECS));

                let Some(fresh_request) = broker::outbound_request(&msg_id).ok().flatten() else {
                    return;
                };

                if !pending_message_exists(&msg_id) {
                    let _ = broker::delete_outbound_request(&msg_id);
                    return;
                }

                let snap2 = ipc::capture_pane(&fresh_request.transport_target);
                if snap1 != snap2 {
                    eprintln!(
                        "sidekar bus: nudge for {msg_id}: recipient still busy, resetting timer"
                    );
                    wait_secs = NUDGE_INTERVAL_SECS;
                    continue;
                }
            }

            let nudge_count = match broker::increment_nudge_count(&msg_id) {
                Ok(count) => count,
                Err(_) => return,
            };
            eprintln!(
                "sidekar bus: nudging recipient for {msg_id} (attempt {nudge_count}/{NUDGE_MAX})"
            );

            let nudge_msg = format!(
                "[sidekar] You have an unanswered request from {}. Reply using bus_send or bus_done with reply_to: \"{msg_id}\"",
                request.sender_label
            );
            let _ = deliver_via(
                &request.transport_name,
                &request.transport_target,
                &nudge_msg,
                "sidekar",
            );

            if nudge_count >= NUDGE_MAX {
                eprintln!("sidekar bus: gave up nudging for {msg_id} after {NUDGE_MAX} attempts");
                return;
            }

            wait_secs = NUDGE_BACKOFF_SECS;
        }
    });
}

/// Build a warning string for unanswered pending messages.
pub fn pending_warnings(state: &SidekarBusState) -> Option<String> {
    let name = state.name()?;
    let pending = broker::pending_for_agent(name).ok()?;
    if pending.is_empty() {
        return None;
    }
    let now = epoch_secs();
    let mut warnings = Vec::new();
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
    if warnings.is_empty() {
        return None;
    }
    Some(format!(
        "\u{26a0} You have {} unanswered message(s). Respond using bus_done or bus_send with reply_to:\n{}",
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
            warnings.push(format!(
                "No response from {} to request {} after {}s.",
                request.recipient_name, request.msg_id, TIMEOUT_SECS
            ));
        }
        let _ = broker::delete_outbound_request(&request.msg_id);
    }

    if warnings.is_empty() {
        None
    } else {
        Some(warnings.join("\n"))
    }
}

fn maybe_track_request(
    state: &mut SidekarBusState,
    envelope: &Envelope,
    delivery: &DeliveryTarget,
) {
    if !matches!(envelope.kind, MessageKind::Request | MessageKind::Handoff) {
        return;
    }
    if let Err(e) = broker::set_pending(envelope) {
        eprintln!(
            "sidekar bus: failed to persist pending {}: {e}",
            envelope.id
        );
        return;
    }
    if let Err(e) = broker::set_outbound_request(
        &envelope.id,
        &envelope.from.name,
        &envelope.from.display_name(),
        &envelope.to,
        delivery.transport_name,
        &delivery.transport_target,
        envelope.created_at,
    ) {
        eprintln!(
            "sidekar bus: failed to persist outbound {}: {e}",
            envelope.id
        );
        return;
    }
    state.ensure_nudge_timer(&envelope.id);
}

fn cleanup_tracking(msg_id: &str) {
    let _ = broker::clear_pending(msg_id);
    let _ = broker::delete_outbound_request(msg_id);
}

fn resolve_reply(reply_to: Option<&str>) {
    if let Some(reply_id) = reply_to {
        let _ = broker::resolve_reply(reply_id);
    }
}

fn find_delivery_target(to: &str, channel: &str) -> Option<DeliveryTarget> {
    if let Some(agent) = find_agent_on_channel(to, channel) {
        return Some(DeliveryTarget {
            transport_name: TMUX_TRANSPORT,
            transport_target: agent.pane_target().to_string(),
            output_label: format!("pane {}", agent.pane_display()),
        });
    }

    ipc::find_agent_socket(to).map(|socket_path| DeliveryTarget {
        transport_name: SOCKET_TRANSPORT,
        transport_target: socket_path.to_string_lossy().to_string(),
        output_label: "cross-session via IPC".to_string(),
    })
}

fn send_directed_envelope(
    state: &mut SidekarBusState,
    ctx: &mut AppContext,
    envelope: Envelope,
    reply_to: Option<&str>,
    verb: &str,
) -> Result<()> {
    let channel = match state.channel() {
        Some(c) => c.to_string(),
        None => bail!("Not running inside tmux."),
    };

    if envelope.to == "@all" {
        broadcast(
            ctx,
            &channel,
            &envelope.from.name,
            &envelope.format_for_paste(),
        )?;
        resolve_reply(reply_to);
        return Ok(());
    }

    let full_message = envelope.format_for_paste();
    let delivery = find_delivery_target(&envelope.to, &channel).ok_or_else(|| {
        let available = available_agents_str(&channel, &envelope.from.name);
        anyhow!("Unknown agent \"{}\". Available on this channel: {available}. Use `who` to see all agents.", envelope.to)
    })?;

    maybe_track_request(state, &envelope, &delivery);

    if let Err(e) = deliver_via(
        delivery.transport_name,
        &delivery.transport_target,
        &full_message,
        &envelope.from.name,
    ) {
        cleanup_tracking(&envelope.id);
        bail!("Failed to reach {}: {e}", envelope.to);
    }

    resolve_reply(reply_to);

    if matches!(envelope.kind, MessageKind::Request | MessageKind::Handoff) {
        out!(
            ctx,
            "{verb} to {} ({}). [msg_id: {}]",
            envelope.to,
            delivery.output_label,
            envelope.id
        );
    } else {
        out!(
            ctx,
            "{verb} to {} ({}).",
            envelope.to,
            delivery.output_label
        );
    }
    Ok(())
}

// --- Tool handlers ---

pub fn cmd_who(state: &SidekarBusState, ctx: &mut AppContext, show_all: bool) -> Result<()> {
    if show_all {
        return crate::ipc::cmd_who(ctx);
    }

    let channel = match state.channel() {
        Some(c) => c.to_string(),
        None => return crate::ipc::cmd_who(ctx),
    };
    let my_name = state.name().unwrap_or("unknown");
    let agents = list_tmux_agents();
    let on_channel: Vec<&TmuxAgent> = agents.iter().filter(|a| a.session() == channel).collect();

    if on_channel.is_empty() {
        out!(ctx, "No agents on channel \"{channel}\".");
        return Ok(());
    }

    let lines: Vec<String> = on_channel
        .iter()
        .map(|a| {
            let you = if a.name() == my_name { " (you)" } else { "" };
            let nick = a.nick().map(|n| format!(" \"{n}\"")).unwrap_or_default();
            format!("- {}{}{} (pane {})", a.name(), nick, you, a.pane_display())
        })
        .collect();
    out!(
        ctx,
        "Channel \"{channel}\":\n{}\n\nUse \"@all\" to broadcast to all agents.",
        lines.join("\n")
    );
    Ok(())
}

pub fn cmd_send_message(
    state: &mut SidekarBusState,
    ctx: &mut AppContext,
    to: &str,
    message: &str,
    kind: &str,
    reply_to: Option<&str>,
) -> Result<()> {
    let from_id = state.agent_id();
    let msg_kind = MessageKind::from_str_lossy(kind);
    let mut envelope = Envelope::new(from_id, to, msg_kind, message);
    if let Some(rt) = reply_to {
        envelope.reply_to = Some(rt.to_string());
    }
    send_directed_envelope(state, ctx, envelope, reply_to, "Message sent")
}

pub fn cmd_signal_done(
    state: &mut SidekarBusState,
    ctx: &mut AppContext,
    next: &str,
    summary: &str,
    request: &str,
    reply_to: Option<&str>,
) -> Result<()> {
    let from_id = state.agent_id();
    let mut envelope = Envelope::new_handoff(from_id, next, summary, request);
    if let Some(rt) = reply_to {
        envelope.reply_to = Some(rt.to_string());
    }
    send_directed_envelope(state, ctx, envelope, reply_to, "Handed off")
}

fn broadcast(ctx: &mut AppContext, channel: &str, my_name: &str, message: &str) -> Result<()> {
    let same_session = agents_on_channel(channel, my_name);
    let cross_session = ipc::discover_all_agents()
        .into_iter()
        .filter(|(_, id)| id.name != my_name && id.nick.as_deref() != Some(my_name))
        .collect::<Vec<_>>();

    if same_session.is_empty() && cross_session.is_empty() {
        bail!("No other agents to broadcast to.");
    }

    let mut delivered: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for agent in &same_session {
        seen.insert(agent.name().to_string());
        match deliver_via(TMUX_TRANSPORT, agent.pane_target(), message, my_name) {
            Ok(()) => delivered.push(agent.name().to_string()),
            Err(_) => failed.push(agent.name().to_string()),
        }
    }

    for (socket_path, id) in &cross_session {
        if seen.contains(&id.name) {
            continue;
        }
        seen.insert(id.name.clone());
        match deliver_via(
            SOCKET_TRANSPORT,
            &socket_path.to_string_lossy(),
            message,
            my_name,
        ) {
            Ok(()) => delivered.push(id.name.clone()),
            Err(_) => failed.push(id.name.clone()),
        }
    }

    let total = delivered.len() + failed.len();
    let mut result = format!("Broadcast to {total} agent(s).");
    if !delivered.is_empty() {
        result.push_str(&format!(" Delivered: {}.", delivered.join(", ")));
    }
    if !failed.is_empty() {
        result.push_str(&format!(" Failed: {}.", failed.join(", ")));
    }
    out!(ctx, "{result}");
    Ok(())
}

pub fn cmd_register(
    state: &mut SidekarBusState,
    ctx: &mut AppContext,
    custom_name: Option<&str>,
) -> Result<()> {
    state.do_register(custom_name);
    match state.name() {
        Some(name) => {
            out!(
                ctx,
                "Registered as \"{name}\" on channel \"{}\".",
                state.channel().unwrap_or("unknown")
            );
            Ok(())
        }
        None => bail!("Registration failed — not running inside tmux."),
    }
}

pub fn cmd_unregister(state: &mut SidekarBusState, ctx: &mut AppContext) -> Result<()> {
    if state.identity.is_none() {
        bail!("Not registered.");
    }
    let old_name = state.name().unwrap_or("unknown").to_string();
    state.unregister();
    out!(ctx, "Unregistered \"{old_name}\". You are now off the bus.");
    Ok(())
}
