//! Sidekar bus — registration, message tracking, and coordination.
//!
//! Provides agent identity management, pending message tracking, nudge
//! timers, broadcast, and bus handoffs. Durable state stored in the broker
//! SQLite database, delivery via the broker message queue.

use crate::broker::{self, BrokerAgent};
use crate::message::{epoch_secs, AgentId, Envelope, MessageKind};
use crate::transport::Transport;
use crate::*;
use std::io::Write as _;

const PENDING_GRACE_SECS: u64 = 30;
const TIMEOUT_SECS: u64 = 300;
const NUDGE_INTERVAL_SECS: u64 = 45;
const NUDGE_BACKOFF_SECS: u64 = 90;
const NUDGE_MAX: u32 = 3;
const NUDGE_POLL_SECS: u64 = 5;
#[allow(dead_code)]
const NUDGE_BUSY_CHECK_SECS: u64 = 3;
const BROKER_TRANSPORT: &str = "broker";

// --- Terminal title helper ---

/// Write an OSC 0 escape sequence to set the terminal title.
/// Tries `/dev/tty` first (works even when stderr is redirected, e.g. in PTY
/// mode), then falls back to stderr.
fn set_terminal_title(title: &str) {
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
/// Uses the full path for consistency with KV store.
fn detect_project_name() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".into())
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

/// Pick a nickname for a project, trying to reuse previous if available.
pub fn pick_nickname_standalone() -> String {
    pick_nickname_for_project(None)
}

pub fn pick_nickname_for_project(project: Option<&str>) -> String {
    use rand::seq::SliceRandom;

    let mut used: HashSet<String> = HashSet::new();
    if let Ok(agents) = broker::list_agents(None) {
        for agent in agents {
            if let Some(nick) = agent.id.nick {
                used.insert(nick);
            }
        }
    }

    // Try to reuse stored nickname for this project (only if not already taken)
    let chosen = if let Some(proj) = project {
        let stored = broker::kv_get(Some(proj), "_agent_nick")
            .ok()
            .flatten()
            .map(|e| e.value);

        if let Some(nick) = stored {
            if !used.contains(&nick) {
                return nick; // reused successfully
            }
            // stored nickname is taken - fall through to pick new one
        }

        // Pick new from available
        let mut available: Vec<&str> = NICKNAMES
            .iter()
            .filter(|n| !used.contains(**n))
            .copied()
            .collect();
        available.shuffle(&mut rand::rng());
        let picked = available.first().map(|s| s.to_string()).unwrap_or_else(|| {
            let r: u16 = rand::random();
            format!("agent-{:04x}", r)
        });

        // Store whatever nickname we got assigned
        if let Err(e) = broker::kv_set(Some(proj), "_agent_nick", &picked) {
            let _ = e;
        }
        picked
    } else {
        // Standalone - no project, just pick random
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
    };

    chosen
}

#[derive(Debug, Clone)]
struct DeliveryTarget {
    transport_name: &'static str,
    transport_target: String,
    output_label: String,
}

#[derive(Debug, Clone, Default)]
pub struct SidekarBusState {
    /// Agent identity (None if not registered).
    pub identity: Option<AgentId>,
    /// Unique pane/session ID (e.g. "pty-12345" or "mcp-12345").
    pub pane_unique_id: Option<String>,
    pub socket_path: Option<PathBuf>,
    /// True when identity was inherited from a parent PTY wrapper.
    /// Skip starting a duplicate IPC socket when inherited.
    pub inherited_pty: bool,
    /// True when identity was borrowed from another process (CLI recovering PTY state).
    /// Drop will NOT unregister — the owning process manages the registration.
    pub borrowed: bool,
    active_nudges: HashSet<String>,
}

impl SidekarBusState {
    pub fn new() -> Self {
        Self {
            identity: None,
            pane_unique_id: None,
            socket_path: None,
            inherited_pty: false,
            borrowed: false,
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
            let _ = broker::touch_agent(name);
        }
    }

    pub fn set_socket_path(&mut self, socket_path: Option<PathBuf>) {
        self.socket_path = socket_path.clone();
        if let Some(name) = self.name() {
            let _ = broker::set_agent_socket_path(name, socket_path.as_deref());
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
            Err(_) => {}
        }
    }

    pub fn unregister(&mut self) {
        if let Some(name) = self.name().map(String::from) {
            let _ = broker::unregister_agent(&name);
        }

        self.identity = None;
        self.pane_unique_id = None;
        self.active_nudges.clear();
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
            set_terminal_title(&format!("{nick} ({name}) — {agent_type}"));
            self.pane_unique_id = inherited.pane.clone();
            self.inherited_pty = true;
            self.identity = Some(inherited);
            self.resume_nudges();
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
        self.active_nudges.clear();

        if let (Some(name), Some(nick), Some(_channel)) = (self.name(), self.nick(), self.channel())
        {
            let agent_type = detect_agent_type();
            set_terminal_title(&format!("{nick} ({name}) — {agent_type}"));
            // registered on bus
        }

        self.resume_nudges();
    }
}

impl Drop for SidekarBusState {
    fn drop(&mut self) {
        if !self.borrowed {
            self.unregister();
        }
    }
}

fn find_agent_on_channel(name_or_nick: &str, channel: &str) -> Option<BrokerAgent> {
    broker::list_agents(Some(channel))
        .unwrap_or_default()
        .into_iter()
        .find(|a| a.id.name == name_or_nick || a.id.nick.as_deref() == Some(name_or_nick))
}

fn agents_on_channel(session: &str, exclude: &str) -> Vec<BrokerAgent> {
    broker::list_agents(Some(session))
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a.id.name != exclude)
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
        BROKER_TRANSPORT => crate::transport::Broker.deliver(target, message, from)?,
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

            let nudge_count = match broker::increment_nudge_count(&msg_id) {
                Ok(count) => count,
                Err(_) => return,
            };

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
                return;
            }

            wait_secs = NUDGE_BACKOFF_SECS;
        }
    });
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
        let _ = e;
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
        let _ = e;
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
    // Try same-channel first, then any agent
    if let Some(agent) = find_agent_on_channel(to, channel) {
        return Some(DeliveryTarget {
            transport_name: BROKER_TRANSPORT,
            transport_target: agent.id.name.clone(),
            output_label: format!("via broker (channel {})", channel),
        });
    }

    // Cross-channel fallback
    if let Ok(Some(agent)) = broker::find_agent(to, None) {
        return Some(DeliveryTarget {
            transport_name: BROKER_TRANSPORT,
            transport_target: agent.id.name.clone(),
            output_label: format!("via broker ({})", agent.id.pane.as_deref().unwrap_or("?")),
        });
    }

    None
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
        None => bail!("Not registered on the bus."),
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
    let my_name = state.name().unwrap_or("unknown");

    let agents = if show_all {
        broker::list_agents(None).unwrap_or_default()
    } else {
        match state.channel() {
            Some(c) => broker::list_agents(Some(c)).unwrap_or_default(),
            None => broker::list_agents(None).unwrap_or_default(),
        }
    };

    if agents.is_empty() {
        let scope = state.channel().unwrap_or("any channel");
        out!(ctx, "No agents on \"{scope}\".");
        return Ok(());
    }

    let channel_label = state.channel().unwrap_or("all");
    let mut lines: Vec<String> = Vec::new();

    for a in &agents {
        let you = if a.id.name == my_name { " (you)" } else { "" };
        let nick =
            a.id.nick
                .as_deref()
                .map(|n| format!(" \"{n}\""))
                .unwrap_or_default();
        let pane = a.id.pane.as_deref().unwrap_or("?");
        let cwd = a
            .cwd
            .as_deref()
            .map(|c| format!(", cwd: {c}"))
            .unwrap_or_default();
        lines.push(format!(
            "- {}{}{} (pane {}{})",
            a.id.name, nick, you, pane, cwd
        ));
    }

    out!(
        ctx,
        "Channel \"{channel_label}\":\n{}\n\nUse \"@all\" to broadcast to all agents.",
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

fn broadcast(ctx: &mut AppContext, _channel: &str, my_name: &str, message: &str) -> Result<()> {
    let all_agents = broker::list_agents(None).unwrap_or_default();
    let targets: Vec<_> = all_agents
        .into_iter()
        .filter(|a| a.id.name != my_name && a.id.nick.as_deref() != Some(my_name))
        .collect();

    if targets.is_empty() {
        bail!("No other agents to broadcast to.");
    }

    let mut delivered: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();

    for agent in &targets {
        match deliver_via(BROKER_TRANSPORT, &agent.id.name, message, my_name) {
            Ok(()) => delivered.push(agent.id.name.clone()),
            Err(_) => failed.push(agent.id.name.clone()),
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
        None => bail!(
            "Registration failed — the agent bus requires a sidekar PTY wrapper.\n\n\
             To fix, launch your agent with: sidekar claude, sidekar codex, etc."
        ),
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
