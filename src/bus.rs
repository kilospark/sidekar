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

const PENDING_GRACE_SECS: u64 = 30;
const TIMEOUT_SECS: u64 = 300;
const BROKER_TRANSPORT: &str = "broker";
const RELAY_HTTP_TRANSPORT: &str = "relay_http";
const DEFAULT_BUS_LIST_LIMIT: usize = 20;

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
        let nick_key = format!("_nick:{}", proj);
        let stored = broker::kv_get(&nick_key).ok().flatten().map(|e| e.value);

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
        let _ = broker::kv_set(&nick_key, &picked);
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
}

impl SidekarBusState {
    pub fn new() -> Self {
        Self {
            identity: None,
            pane_unique_id: None,
            socket_path: None,
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

    pub fn set_socket_path(&mut self, socket_path: Option<PathBuf>) {
        self.socket_path = socket_path.clone();
        if let Some(name) = self.name() {
            let _ = broker::set_agent_socket_path(name, socket_path.as_deref());
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
            set_terminal_title(&format!("{nick} ({name}) — {agent_type}"));
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
            set_terminal_title(&format!("{nick} ({name}) — {agent_type}"));
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
        RELAY_HTTP_TRANSPORT => crate::transport::RelayHttp.deliver(target, message, from)?,
        other => bail!("unknown transport: {other}"),
    };
    match result {
        crate::message::DeliveryResult::Delivered | crate::message::DeliveryResult::Queued => {
            Ok(())
        }
        crate::message::DeliveryResult::Failed(reason) => bail!("delivery failed: {reason}"),
    }
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

fn maybe_track_request(state: &SidekarBusState, envelope: &Envelope, delivery: &DeliveryTarget) {
    if !matches!(envelope.kind, MessageKind::Request | MessageKind::Handoff) {
        return;
    }
    if let Err(e) = broker::set_pending(envelope) {
        let _ = e;
        return;
    }
    let project = detect_project_name();
    if let Err(e) = broker::set_outbound_request(
        envelope,
        &envelope.from.display_name(),
        delivery.transport_name,
        &delivery.transport_target,
        state.channel(),
        Some(project.as_str()),
    ) {
        let _ = e;
    }
    let _ =
        broker::mark_agent_session_request(&envelope.from.name, &envelope.id, envelope.created_at);
}

fn cleanup_tracking(msg_id: &str) {
    let _ = broker::clear_pending(msg_id);
    let _ = broker::delete_outbound_request(msg_id);
}

fn resolve_reply(envelope: &Envelope, reply_to: Option<&str>) {
    if let Some(reply_id) = reply_to {
        let _ = broker::record_reply(reply_id, envelope);
    }
}

fn clear_local_pending_reply(reply_to: Option<&str>) {
    if let Some(reply_id) = reply_to {
        let _ = broker::resolve_reply(reply_id);
    }
}

fn cleanup_completed_exchange(
    self_name: &str,
    other_name: &str,
    channel: Option<&str>,
    keep_msg_id: Option<&str>,
) {
    let canonical_other = broker::find_agent(other_name, channel)
        .ok()
        .flatten()
        .map(|agent| agent.id.name)
        .unwrap_or_else(|| other_name.to_string());
    let _ = broker::clear_pending_between_agents(self_name, &canonical_other);
    let _ = broker::close_outbound_between_agents(self_name, &canonical_other, keep_msg_id);
}

fn relay_session_for_target(to: &str) -> Option<crate::transport::RelaySessionInfo> {
    if crate::auth::auth_token().is_none() {
        return None;
    }
    let sessions = crate::transport::fetch_relay_sessions().ok()?;
    sessions
        .into_iter()
        .find(|s| s.name == to || s.nickname.as_deref() == Some(to))
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

    // Remote relay: another machine / session for this user (device token + live tunnel on relay)
    if let Some(sess) = relay_session_for_target(to) {
        return Some(DeliveryTarget {
            transport_name: RELAY_HTTP_TRANSPORT,
            transport_target: sess.id.clone(),
            output_label: format!("via relay ({})", sess.hostname),
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
        None => bail!("Not registered on the bus. Relaunch your agent with: sidekar <agent-cli>"),
    };

    if matches!(envelope.to.as_str(), "@all" | "all") {
        bail!("Broadcast targets are not supported. Use `sidekar bus who` and message a specific agent.");
    }

    let full_message = envelope.format_for_paste();
    let delivery = find_delivery_target(&envelope.to, &channel).ok_or_else(|| {
        let available = available_agents_str(&channel, &envelope.from.name);
        anyhow!(
            "Unknown agent \"{}\". Available on this channel: {available}. Use `sidekar bus who` to see all agents.",
            envelope.to
        )
    })?;

    maybe_track_request(state, &envelope, &delivery);

    let delivery_result: Result<()> = if delivery.transport_name == RELAY_HTTP_TRANSPORT {
        crate::transport::deliver_relay_envelope(&delivery.transport_target, &envelope).map(|_| ())
    } else {
        deliver_via(
            delivery.transport_name,
            &delivery.transport_target,
            &full_message,
            &envelope.from.name,
        )
    };

    if let Err(e) = delivery_result {
        cleanup_tracking(&envelope.id);
        bail!("Failed to reach {}: {e}", envelope.to);
    }

    if delivery.transport_name == RELAY_HTTP_TRANSPORT {
        clear_local_pending_reply(reply_to);
    } else {
        resolve_reply(&envelope, reply_to);
    }

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
        "Channel \"{channel_label}\":\n{}",
        lines.join("\n")
    );
    Ok(())
}

pub fn cmd_requests(
    state: &SidekarBusState,
    ctx: &mut AppContext,
    status: Option<&str>,
    limit: usize,
) -> Result<()> {
    let limit = if limit == 0 {
        DEFAULT_BUS_LIST_LIMIT
    } else {
        limit
    };
    let self_name = state
        .name()
        .ok_or_else(|| anyhow!("Not registered on the bus. Relaunch your agent with: sidekar <agent-cli>"))?;
    let requests = broker::list_outbound_requests_for_sender(self_name, status, limit)?;
    if requests.is_empty() {
        out!(ctx, "No outbound requests.");
        return Ok(());
    }

    out!(
        ctx,
        "msg_id\tstatus\tkind\tto\tcreated_at\tanswered_at\tpreview"
    );
    for request in requests {
        out!(
            ctx,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            request.msg_id,
            request.status,
            request.kind,
            request.recipient_name,
            request.created_at,
            request
                .answered_at
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".into()),
            request.message_preview.replace('\n', " "),
        );
    }
    Ok(())
}

pub fn cmd_replies(
    state: &SidekarBusState,
    ctx: &mut AppContext,
    reply_to_msg_id: Option<&str>,
    limit: usize,
) -> Result<()> {
    let limit = if limit == 0 {
        DEFAULT_BUS_LIST_LIMIT
    } else {
        limit
    };
    let self_name = state
        .name()
        .ok_or_else(|| anyhow!("Not registered on the bus. Relaunch your agent with: sidekar <agent-cli>"))?;
    let replies = broker::list_bus_replies_for_sender(self_name, reply_to_msg_id, limit)?;
    if replies.is_empty() {
        out!(ctx, "No replies.");
        return Ok(());
    }

    out!(
        ctx,
        "reply_to\treply_id\tfrom\tkind\tcreated_at\tmessage"
    );
    for reply in replies {
        out!(
            ctx,
            "{}\t{}\t{}\t{}\t{}\t{}",
            reply.reply_to_msg_id,
            reply.reply_msg_id,
            reply.sender_label,
            reply.kind,
            reply.created_at,
            reply.message.replace('\n', " "),
        );
    }
    Ok(())
}

pub fn cmd_show_request(
    state: &SidekarBusState,
    ctx: &mut AppContext,
    msg_id: &str,
) -> Result<()> {
    let self_name = state
        .name()
        .ok_or_else(|| anyhow!("Not registered on the bus. Relaunch your agent with: sidekar <agent-cli>"))?;
    let request = broker::outbound_request(msg_id)?
        .ok_or_else(|| anyhow!("Unknown request: {msg_id}"))?;
    if request.sender_name != self_name {
        bail!("Request {msg_id} does not belong to the current agent.");
    }

    out!(ctx, "msg_id: {}", request.msg_id);
    out!(ctx, "status: {}", request.status);
    out!(ctx, "kind: {}", request.kind);
    out!(ctx, "to: {}", request.recipient_name);
    out!(ctx, "channel: {}", request.channel.as_deref().unwrap_or("-"));
    out!(ctx, "project: {}", request.project.as_deref().unwrap_or("-"));
    out!(ctx, "created_at: {}", request.created_at);
    out!(
        ctx,
        "answered_at: {}",
        request
            .answered_at
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into())
    );
    out!(
        ctx,
        "timed_out_at: {}",
        request
            .timed_out_at
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into())
    );
    out!(ctx, "preview: {}", request.message_preview);

    let replies = broker::replies_for_request(msg_id)?;
    if replies.is_empty() {
        out!(ctx, "replies: none");
        return Ok(());
    }

    out!(ctx, "replies:");
    for reply in replies {
        out!(
            ctx,
            "- {} {} {} {}",
            reply.reply_msg_id,
            reply.kind,
            reply.sender_label,
            reply.message.replace('\n', " ")
        );
    }
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
    let self_name = from_id.name.clone();
    let channel = state.channel().map(str::to_string);
    let mut envelope = Envelope::new_handoff(from_id, next, summary, request);
    if let Some(rt) = reply_to {
        envelope.reply_to = Some(rt.to_string());
    }
    let keep_msg_id = envelope.id.clone();
    send_directed_envelope(state, ctx, envelope, reply_to, "Handed off")?;
    cleanup_completed_exchange(&self_name, next, channel.as_deref(), Some(&keep_msg_id));
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
