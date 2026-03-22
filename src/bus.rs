//! Sidekar bus — registration, message tracking, and coordination.
//!
//! Provides agent identity management, pending message tracking, nudge
//! timers, broadcast, and bus handoffs. Built on top of the ipc module's
//! socket and paste primitives.

use crate::ipc;
use crate::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
            // Node-based CLIs
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
    // Try git repo root name
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

    // Fall back to cwd basename
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
    "badger", "bantam", "barbet", "basilisk", "bison", "bobcat", "bonobo", "borzoi",
    "caiman", "capybara", "caracal", "cassowary", "cheetah", "chinchilla", "cicada", "civet",
    "coati", "condor", "corgi", "cougar", "coyote", "crane", "cuckoo", "curlew",
    "dingo", "dormouse", "drongo", "dugong", "dunlin",
    "egret", "ermine", "falcon", "fennec", "ferret", "finch", "flamingo", "flounder",
    "gannet", "gazelle", "gecko", "gerbil", "gibbon", "gopher", "grouse", "guppy",
    "harrier", "hedgehog", "heron", "hoopoe", "hornet", "husky", "hyena",
    "ibis", "iguana", "impala", "jackal", "jackdaw", "jaguar", "jerboa",
    "kakapo", "kestrel", "kinkajou", "kiwi", "kodiak", "komodo",
    "lemur", "leopard", "limpet", "loris", "macaw", "mako", "mamba", "mandrill",
    "mantis", "margay", "marlin", "marmot", "merlin", "mink", "mongoose", "moray",
    "narwhal", "newt", "numbat", "ocelot", "okapi", "oriole", "osprey", "otter",
    "pangolin", "parrot", "pelican", "penguin", "peregrine", "pika", "piranha", "platypus",
    "quail", "quetzal", "quokka", "raven", "robin", "rooster",
    "sable", "salmon", "scarab", "serval", "shrike", "sparrow", "starling", "stoat",
    "taipan", "tamarin", "tanager", "tarpon", "tenrec", "tern", "thrush", "toucan",
    "uakari", "umbrellabird", "viper", "vizsla", "vulture",
    "wallaby", "walrus", "weasel", "whippet", "wombat", "woodpecker",
    "xerus", "yak", "zebu", "zorilla",
];

/// Pick a random unused nickname from the pool.
fn pick_nickname() -> String {
    use rand::seq::SliceRandom;
    let used: HashSet<String> = list_tmux_agents()
        .into_iter()
        .filter_map(|a| a.nick)
        .collect();
    let mut available: Vec<&str> = NICKNAMES
        .iter()
        .filter(|n| !used.contains(**n))
        .copied()
        .collect();
    available.shuffle(&mut rand::rng());
    available
        .first()
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            let r: u16 = rand::random();
            format!("agent-{:04x}", r)
        })
}

// --- Base64 for tmux pane option storage ---

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((n >> 18) & 63) as usize] as char);
        result.push(CHARS[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((n >> 6) & 63) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(n & 63) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            b'=' => Some(0),
            _ => None,
        }
    }
    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut result = Vec::new();
    for chunk in bytes.chunks(4) {
        if chunk.len() < 4 {
            return None;
        }
        let n = (val(chunk[0])? << 18)
            | (val(chunk[1])? << 12)
            | (val(chunk[2])? << 6)
            | val(chunk[3])?;
        result.push(((n >> 16) & 255) as u8);
        if chunk[2] != b'=' {
            result.push(((n >> 8) & 255) as u8);
        }
        if chunk[3] != b'=' {
            result.push((n & 255) as u8);
        }
    }
    Some(result)
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn gen_msg_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let r: u16 = rand::random();
    format!("{:x}-{:04x}", ts & 0xFFFF_FFFF, r)
}

// --- Pending message tracking ---

fn set_pending(recipient_pane: &str, msg_id: &str, envelope: &Value) {
    let key = format!("@sidekar-pending-{msg_id}");
    let json_str = serde_json::to_string(envelope).unwrap_or_default();
    let val = base64_encode(json_str.as_bytes());
    let _ = Command::new("tmux")
        .args(["set-option", "-p", "-t", recipient_pane, &key, &val])
        .status();
}

fn clear_pending(pane: &str, msg_id: &str) {
    let key = format!("@sidekar-pending-{msg_id}");
    let _ = Command::new("tmux")
        .args(["set-option", "-pu", "-t", pane, &key])
        .status();
}

fn read_pending(pane: &str) -> Vec<(String, Value)> {
    let output = Command::new("tmux")
        .args(["show-options", "-p", "-t", pane])
        .output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut pending = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("@sidekar-pending-") {
            if let Some((id, b64)) = rest.split_once(' ') {
                let b64 = b64.trim().trim_matches('"');
                if let Some(bytes) = base64_decode(b64) {
                    if let Ok(json_str) = String::from_utf8(bytes) {
                        if let Ok(val) = serde_json::from_str::<Value>(&json_str) {
                            pending.push((id.to_string(), val));
                        }
                    }
                }
            }
        }
    }
    pending
}

const PENDING_GRACE_SECS: u64 = 30;

/// Build a warning string for unanswered pending messages.
pub fn pending_warnings(pane: &str) -> Option<String> {
    let pending = read_pending(pane);
    if pending.is_empty() {
        return None;
    }
    let now = epoch_secs();
    let mut warnings = Vec::new();
    for (id, env) in &pending {
        let created = env
            .get("created_at")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        if created > 0 && now.saturating_sub(created) < PENDING_GRACE_SECS {
            continue;
        }
        let from = env.get("from").and_then(Value::as_str).unwrap_or("?");
        let kind = env.get("kind").and_then(Value::as_str).unwrap_or("request");
        let msg = env
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| env.get("request").and_then(Value::as_str))
            .unwrap_or("");
        let preview = if msg.len() > 100 {
            let mut end = 100;
            while end > 0 && !msg.is_char_boundary(end) {
                end -= 1;
            }
            &msg[..end]
        } else {
            msg
        };
        warnings.push(format!("  [{kind} id={id} from {from}]: {preview}"));
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

// --- Outbound tracking ---

pub struct OutboundRequest {
    pub msg_id: String,
    pub recipient_pane: String,
    pub created: Instant,
    pub nudge_cancel: Arc<AtomicBool>,
}

const TIMEOUT_SECS: u64 = 300;

pub fn check_outbound_timeouts(outbound: &mut Vec<OutboundRequest>) -> Option<String> {
    let mut warnings = Vec::new();
    outbound.retain(|req| {
        if req.created.elapsed().as_secs() < TIMEOUT_SECS {
            return true;
        }
        let pending = read_pending(&req.recipient_pane);
        let still_pending = pending.iter().any(|(id, _)| *id == req.msg_id);
        if still_pending {
            clear_pending(&req.recipient_pane, &req.msg_id);
            let who = pending
                .iter()
                .find(|(id, _)| *id == req.msg_id)
                .and_then(|(_, env)| env.get("to").and_then(Value::as_str).map(String::from))
                .unwrap_or_else(|| "recipient".to_string());
            warnings.push(format!(
                "No response from {} to request {} after {}s.",
                who, req.msg_id, TIMEOUT_SECS
            ));
        }
        req.nudge_cancel.store(true, Ordering::Relaxed);
        false
    });
    if warnings.is_empty() {
        None
    } else {
        Some(warnings.join("\n"))
    }
}

// --- Nudge timer ---

const NUDGE_INTERVAL_SECS: u64 = 45;
const NUDGE_BACKOFF_SECS: u64 = 90;
const NUDGE_MAX: u32 = 3;
const NUDGE_POLL_SECS: u64 = 5;
const NUDGE_BUSY_CHECK_SECS: u64 = 3;

fn spawn_nudge_timer(
    recipient_pane: String,
    msg_id: String,
    sender_name: String,
    cancel: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let mut nudge_count = 0u32;
        let mut wait_secs = NUDGE_INTERVAL_SECS;

        loop {
            let mut elapsed = 0u64;
            while elapsed < wait_secs {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_secs(NUDGE_POLL_SECS));
                elapsed += NUDGE_POLL_SECS;
            }

            if cancel.load(Ordering::Relaxed) {
                return;
            }

            let pending = read_pending(&recipient_pane);
            if !pending.iter().any(|(id, _)| *id == msg_id) {
                return;
            }

            let snap1 = ipc::capture_pane(&recipient_pane);
            std::thread::sleep(Duration::from_secs(NUDGE_BUSY_CHECK_SECS));

            if cancel.load(Ordering::Relaxed) {
                return;
            }

            let snap2 = ipc::capture_pane(&recipient_pane);
            if snap1 != snap2 {
                eprintln!("sidekar bus: nudge for {msg_id}: recipient still busy, resetting timer");
                wait_secs = NUDGE_INTERVAL_SECS;
                continue;
            }

            let pending = read_pending(&recipient_pane);
            if !pending.iter().any(|(id, _)| *id == msg_id) {
                return;
            }

            nudge_count += 1;
            eprintln!(
                "sidekar bus: nudging recipient for {msg_id} (attempt {nudge_count}/{NUDGE_MAX})"
            );

            let nudge_msg = format!(
                "[sidekar] You have an unanswered request from {sender_name}. \
                 Reply using bus_send or bus_done with reply_to: \"{msg_id}\""
            );
            let _ = ipc::send_to_pane(&recipient_pane, &nudge_msg);

            if nudge_count >= NUDGE_MAX {
                eprintln!("sidekar bus: gave up nudging for {msg_id} after {NUDGE_MAX} attempts");
                return;
            }

            wait_secs = NUDGE_BACKOFF_SECS;
        }
    });
}

// --- Agent state ---

pub struct SidekarBusState {
    pub name: Option<String>,
    pub nick: Option<String>,
    pub pane: Option<String>,           // display pane ID, e.g. "0:0.1"
    pub pane_unique_id: Option<String>, // unique pane ID, e.g. "%42"
    pub channel: Option<String>,
    pub outbound: Vec<OutboundRequest>,
}

impl SidekarBusState {
    pub fn new() -> Self {
        Self {
            name: None,
            nick: None,
            pane: None,
            pane_unique_id: None,
            channel: None,
            outbound: Vec::new(),
        }
    }

    pub fn unregister(&mut self) {
        // Cancel all nudge timers
        for req in &self.outbound {
            req.nudge_cancel.store(true, Ordering::Relaxed);
        }
        self.outbound.clear();

        if let Some(pane) = &self.pane {
            // Clear any stale pending messages so the next agent on this pane
            // doesn't inherit them.
            for (id, _) in read_pending(pane) {
                clear_pending(pane, &id);
            }
            let _ = Command::new("tmux")
                .args(["set-option", "-pu", "-t", pane, "@agent-name"])
                .status();
            let _ = Command::new("tmux")
                .args(["set-option", "-pu", "-t", pane, "@agent-nick"])
                .status();
            if let Some(name) = &self.name {
                eprintln!("sidekar bus: unregistered \"{name}\"");
            }
        }
        self.name = None;
        self.nick = None;
        self.pane_unique_id = None;
    }

    pub fn do_register(&mut self, custom_name: Option<&str>) {
        if self.name.is_some() {
            self.unregister();
        }

        let (pane, session) = match (&self.pane, &self.channel) {
            (Some(p), Some(s)) => (p.clone(), s.clone()),
            _ => match ipc::detect_tmux_pane() {
                Some(detected) => {
                    self.pane = Some(detected.display_id.clone());
                    self.pane_unique_id = Some(detected.unique_id.clone());
                    self.channel = Some(detected.session.clone());
                    (detected.display_id, detected.session)
                }
                None => return,
            },
        };

        let name = if let Some(custom) = custom_name {
            custom.to_string()
        } else {
            let agent_type = detect_agent_type();
            let project = detect_project_name();
            // Include project name in agent name for global uniqueness and clarity
            let existing_names: HashSet<String> =
                list_tmux_agents().into_iter().map(|a| a.name).collect();
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
            .args(["set-option", "-p", "-t", &pane, "@agent-name", &name])
            .status();
        let _ = Command::new("tmux")
            .args(["set-option", "-p", "-t", &pane, "@agent-nick", &nick])
            .status();

        // Always set pane border format (shows nickname prominently)
        let _ = Command::new("tmux")
            .args([
                "set-option",
                "-p",
                "-t",
                &pane,
                "pane-border-format",
                " #{@agent-nick} (#{@agent-name}) | #{pane_title} ",
            ])
            .status();

        // Enable pane borders if not already on
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

        eprintln!(
            "sidekar bus: registered as \"{name}\" aka \"{nick}\" on channel \"{session}\" (pane {pane})"
        );
        self.name = Some(name);
        self.nick = Some(nick);
    }
}

impl Drop for SidekarBusState {
    fn drop(&mut self) {
        self.unregister();
    }
}

struct TmuxAgent {
    name: String,
    nick: Option<String>,
    pane: String,
    session: String,
}

fn list_tmux_agents() -> Vec<TmuxAgent> {
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{@agent-name}\t#{session_name}:#{window_index}.#{pane_index}\t#{session_name}\t#{@agent-nick}",
        ])
        .output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.trim()
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 3 && !parts[0].is_empty() {
                let nick = parts.get(3).and_then(|n| {
                    if n.is_empty() {
                        None
                    } else {
                        Some(n.to_string())
                    }
                });
                Some(TmuxAgent {
                    name: parts[0].to_string(),
                    nick,
                    pane: parts[1].to_string(),
                    session: parts[2].to_string(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Find an agent by name or nickname within a channel.
fn find_agent_pane(name_or_nick: &str, channel: &str) -> Option<String> {
    let agents = list_tmux_agents();
    // Try exact name match first
    if let Some(a) = agents
        .iter()
        .find(|a| a.name == name_or_nick && a.session == channel)
    {
        return Some(a.pane.clone());
    }
    // Try nickname match
    agents
        .iter()
        .find(|a| a.nick.as_deref() == Some(name_or_nick) && a.session == channel)
        .map(|a| a.pane.clone())
}

fn agents_on_channel(session: &str, exclude: &str) -> Vec<TmuxAgent> {
    list_tmux_agents()
        .into_iter()
        .filter(|a| a.session == session && a.name != exclude)
        .collect()
}

fn available_agents_str(channel: &str, exclude: &str) -> String {
    let agents = agents_on_channel(channel, exclude);
    if agents.is_empty() {
        "none".to_string()
    } else {
        agents
            .iter()
            .map(|a| crate::ipc::format_from_label(a.nick.as_deref(), &a.name))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

// --- Tool handlers ---

pub fn cmd_who(state: &SidekarBusState, ctx: &mut AppContext, show_all: bool) -> Result<()> {
    if show_all {
        // Cross-session discovery via IPC sockets
        return crate::ipc::cmd_who(ctx);
    }

    let channel = match &state.channel {
        Some(c) => c,
        None => {
            // Fall back to IPC discovery
            return crate::ipc::cmd_who(ctx);
        }
    };
    let my_name = state.name.as_deref().unwrap_or("unknown");
    let agents = list_tmux_agents();
    let on_channel: Vec<&TmuxAgent> = agents.iter().filter(|a| a.session == *channel).collect();

    if on_channel.is_empty() {
        out!(ctx, "No agents on channel \"{channel}\".");
        return Ok(());
    }

    let lines: Vec<String> = on_channel
        .iter()
        .map(|a| {
            let you = if a.name == my_name { " (you)" } else { "" };
            let nick = a
                .nick
                .as_deref()
                .map(|n| format!(" \"{n}\""))
                .unwrap_or_default();
            format!("- {}{}{} (pane {})", a.name, nick, you, a.pane)
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
    let channel = match &state.channel {
        Some(c) => c.clone(),
        None => bail!("Not running inside tmux."),
    };
    let my_name = state.name.as_deref().unwrap_or("unknown").to_string();
    let msg_id = gen_msg_id();
    let from_label = crate::ipc::format_from_label(state.nick.as_deref(), &my_name);
    let full_message = format!("[message from {from_label}]: {message}");

    if to == "@all" {
        broadcast(ctx, &channel, &my_name, &full_message)?;
        if let (Some(reply_id), Some(pane)) = (reply_to, &state.pane) {
            clear_pending(pane, reply_id);
        }
        return Ok(());
    }

    // Try same-session first
    if let Some(pane) = find_agent_pane(to, &channel) {
        if kind == "request" {
            let envelope = json!({
                "id": msg_id,
                "from": from_label,
                "to": to,
                "kind": "request",
                "message": message,
                "created_at": epoch_secs().to_string(),
            });
            set_pending(&pane, &msg_id, &envelope);

            let cancel = Arc::new(AtomicBool::new(false));
            spawn_nudge_timer(
                pane.clone(),
                msg_id.clone(),
                from_label.clone(),
                cancel.clone(),
            );
            state.outbound.push(OutboundRequest {
                msg_id: msg_id.clone(),
                recipient_pane: pane.clone(),
                created: Instant::now(),
                nudge_cancel: cancel,
            });
        }

        match ipc::send_to_pane(&pane, &full_message) {
            Ok(()) => {
                if kind == "request" {
                    out!(
                        ctx,
                        "Message sent to {to} (pane {pane}). [msg_id: {msg_id}]"
                    );
                } else {
                    out!(ctx, "Message sent to {to} (pane {pane}).");
                }
            }
            Err(e) => {
                // Clean up pending on failure so it doesn't become a ghost
                if kind == "request" {
                    clear_pending(&pane, &msg_id);
                    if let Some(req) = state.outbound.iter().find(|r| r.msg_id == msg_id) {
                        req.nudge_cancel.store(true, Ordering::Relaxed);
                    }
                    state.outbound.retain(|r| r.msg_id != msg_id);
                }
                bail!("Failed to reach {to}: {e}");
            }
        }

        if let (Some(reply_id), Some(my_pane)) = (reply_to, &state.pane) {
            clear_pending(my_pane, reply_id);
        }
        return Ok(());
    }

    // Cross-session via IPC socket
    if let Some(socket_path) = ipc::find_agent_socket(to) {
        ipc::ipc_send_message(&socket_path, &full_message, &my_name)?;
        if let (Some(reply_id), Some(my_pane)) = (reply_to, &state.pane) {
            clear_pending(my_pane, reply_id);
        }
        out!(ctx, "Message sent to {to} (cross-session via IPC).");
        return Ok(());
    }

    let available = available_agents_str(&channel, &my_name);
    bail!("Unknown agent \"{to}\". Available on this channel: {available}. Use `who` to see all agents.");
}

pub fn cmd_signal_done(
    state: &mut SidekarBusState,
    ctx: &mut AppContext,
    next: &str,
    summary: &str,
    request: &str,
    reply_to: Option<&str>,
) -> Result<()> {
    let channel = match &state.channel {
        Some(c) => c.clone(),
        None => bail!("Not running inside tmux."),
    };
    let my_name = state.name.as_deref().unwrap_or("unknown").to_string();
    let msg_id = gen_msg_id();
    let from_label = crate::ipc::format_from_label(state.nick.as_deref(), &my_name);
    let message = format!("[from {from_label}]: {summary} Request: {request} [msg_id: {msg_id}]");

    if next == "@all" {
        broadcast(ctx, &channel, &my_name, &message)?;
        if let (Some(reply_id), Some(pane)) = (reply_to, &state.pane) {
            clear_pending(pane, reply_id);
        }
        return Ok(());
    }

    match find_agent_pane(next, &channel) {
        Some(pane) => {
            let envelope = json!({
                "id": msg_id,
                "from": from_label,
                "to": next,
                "kind": "handoff",
                "message": message,
                "summary": summary,
                "request": request,
                "created_at": epoch_secs().to_string(),
            });
            set_pending(&pane, &msg_id, &envelope);

            let cancel = Arc::new(AtomicBool::new(false));
            spawn_nudge_timer(
                pane.clone(),
                msg_id.clone(),
                from_label.clone(),
                cancel.clone(),
            );
            state.outbound.push(OutboundRequest {
                msg_id: msg_id.clone(),
                recipient_pane: pane.clone(),
                created: Instant::now(),
                nudge_cancel: cancel,
            });

            match ipc::send_to_pane(&pane, &message) {
                Ok(()) => {
                    out!(
                        ctx,
                        "Handed off to {next} (pane {pane}). [msg_id: {msg_id}]"
                    );
                }
                Err(e) => {
                    clear_pending(&pane, &msg_id);
                    if let Some(req) = state.outbound.iter().find(|r| r.msg_id == msg_id) {
                        req.nudge_cancel.store(true, Ordering::Relaxed);
                    }
                    state.outbound.retain(|r| r.msg_id != msg_id);
                    bail!("Failed to reach {next}: {e}");
                }
            }

            if let (Some(reply_id), Some(my_pane)) = (reply_to, &state.pane) {
                clear_pending(my_pane, reply_id);
            }
            Ok(())
        }
        None => {
            // Cross-session via IPC
            if let Some(socket_path) = ipc::find_agent_socket(next) {
                ipc::ipc_send_message(&socket_path, &message, &my_name)?;
                if let (Some(reply_id), Some(my_pane)) = (reply_to, &state.pane) {
                    clear_pending(my_pane, reply_id);
                }
                out!(
                    ctx,
                    "Handed off to {next} (cross-session via IPC). [msg_id: {msg_id}]"
                );
                return Ok(());
            }
            let available = available_agents_str(&channel, &my_name);
            bail!("Unknown agent \"{next}\". Available: {available}.")
        }
    }
}

fn broadcast(ctx: &mut AppContext, channel: &str, my_name: &str, message: &str) -> Result<()> {
    let same_session = agents_on_channel(channel, my_name);
    let cross_session: Vec<(std::path::PathBuf, serde_json::Value)> = ipc::discover_all_agents()
        .into_iter()
        .filter(|(_, info)| {
            let agent = info.get("agent").and_then(Value::as_str);
            let nick = info.get("nick").and_then(Value::as_str);
            agent != Some(my_name) && nick != Some(my_name)
        })
        .collect();

    if same_session.is_empty() && cross_session.is_empty() {
        bail!("No other agents to broadcast to.");
    }

    let mut delivered = Vec::new();
    let mut failed = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for agent in &same_session {
        seen.insert(agent.name.as_str());
        match ipc::send_to_pane(&agent.pane, message) {
            Ok(()) => delivered.push(agent.name.as_str()),
            Err(_) => failed.push(agent.name.as_str()),
        }
    }

    for (socket_path, info) in &cross_session {
        let name = info.get("agent").and_then(Value::as_str).unwrap_or("?");
        if seen.contains(name) {
            continue;
        }
        seen.insert(name);
        match ipc::ipc_send_message(socket_path, message, my_name) {
            Ok(()) => delivered.push(name),
            Err(_) => failed.push(name),
        }
    }

    let total = delivered.len() + failed.len();
    let mut result = format!("Broadcast to {total} agent(s).",);
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
    match &state.name {
        Some(name) => {
            out!(
                ctx,
                "Registered as \"{name}\" on channel \"{}\".",
                state.channel.as_deref().unwrap_or("unknown")
            );
            Ok(())
        }
        None => bail!("Registration failed — not running inside tmux."),
    }
}

pub fn cmd_unregister(state: &mut SidekarBusState, ctx: &mut AppContext) -> Result<()> {
    if state.name.is_none() {
        bail!("Not registered.");
    }
    let old_name = state.name.clone().unwrap_or_default();
    state.unregister();
    out!(ctx, "Unregistered \"{old_name}\". You are now off the bus.");
    Ok(())
}
