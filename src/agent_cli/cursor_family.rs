//! Cursor `cursor` shim vs `agent` / `cursor-agent` argv shaping.

use super::{AgentCliSpec, ProxyEnvFlags, STARTUP_INJECT};

/// Subcommands that are not an interactive agent session (no initial prompt slot).
const MGMT_COMMANDS: &[&str] = &[
    "install-shell-integration",
    "uninstall-shell-integration",
    "login",
    "logout",
    "mcp",
    "status",
    "whoami",
    "models",
    "about",
    "update",
    "create-chat",
    "generate-rule",
    "rule",
    "ls",
    "resume",
    "help",
];

fn skip_one_arg(args: &[String], i: usize) -> usize {
    let a = args[i].as_str();
    if a.contains('=') {
        return i + 1;
    }
    let needs_value = matches!(
        a,
        "--api-key"
            | "-H"
            | "--header"
            | "--output-format"
            | "--mode"
            | "--model"
            | "--sandbox"
            | "--workspace"
            | "--worktree-base"
    );
    if needs_value {
        if i + 1 < args.len() && !args[i + 1].starts_with('-') {
            return i + 2;
        }
        return i + 1;
    }
    if a == "-w" || a == "--worktree" {
        if i + 1 < args.len() && !args[i + 1].starts_with('-') {
            return i + 2;
        }
        return i + 1;
    }
    if a == "--resume" {
        if i + 1 < args.len() && !args[i + 1].starts_with('-') {
            return i + 2;
        }
        return i + 1;
    }
    i + 1
}

fn first_positional_index(args: &[String]) -> usize {
    let mut i = 0usize;
    while i < args.len() {
        if args[i].starts_with('-') {
            i = skip_one_arg(args, i);
        } else {
            return i;
        }
    }
    args.len()
}

fn should_inject_initial_prompt(args: &[String]) -> bool {
    let i = first_positional_index(args);
    if i >= args.len() {
        return true;
    }
    let cmd = args[i].as_str();
    if MGMT_COMMANDS.contains(&cmd) {
        return false;
    }
    if cmd == "agent" {
        let after = &args[i + 1..];
        let j = first_positional_index(after);
        return j >= after.len();
    }
    false
}

/// `sidekar cursor …` (shim that may forward to IDE or `agent`).
pub fn enrich_cursor(user_args: &[String]) -> Vec<String> {
    let has_positional = user_args.iter().any(|a| !a.starts_with('-'));
    if user_args.is_empty() {
        return vec!["agent".into(), STARTUP_INJECT.to_string()];
    }
    if user_args.first().map(|s| s.as_str()) == Some("agent") {
        let mut o = user_args.to_vec();
        if should_inject_initial_prompt(&user_args[1..]) {
            o.push(STARTUP_INJECT.to_string());
        }
        return o;
    }
    let mut out = user_args.to_vec();
    if !has_positional {
        out.push(STARTUP_INJECT.to_string());
    }
    out
}

/// `sidekar agent …` and `sidekar cursor-agent …`.
pub fn enrich_agent_binary(user_args: &[String]) -> Vec<String> {
    let mut out = user_args.to_vec();
    if should_inject_initial_prompt(user_args) {
        out.push(STARTUP_INJECT.to_string());
    }
    out
}

// --- One registry entry per binary; proxy flags are independent per struct. ---

pub struct Cursor;

impl AgentCliSpec for Cursor {
    fn ids(&self) -> &'static [&'static str] {
        &["cursor"]
    }

    fn enrich_startup(&self, invoked_as: &str, user_args: &[String]) -> Vec<String> {
        debug_assert_eq!(invoked_as, "cursor");
        enrich_cursor(user_args)
    }

    fn proxy_env_flags(&self, _invoked_as: &str) -> ProxyEnvFlags {
        // `cursor` shim: universal MITM + CA only (IDE vs agent subcommand varies at runtime).
        ProxyEnvFlags::default()
    }
}

pub struct Agent;

impl AgentCliSpec for Agent {
    fn ids(&self) -> &'static [&'static str] {
        &["agent"]
    }

    fn enrich_startup(&self, invoked_as: &str, user_args: &[String]) -> Vec<String> {
        debug_assert_eq!(invoked_as, "agent");
        enrich_agent_binary(user_args)
    }

    fn proxy_env_flags(&self, _invoked_as: &str) -> ProxyEnvFlags {
        ProxyEnvFlags::default()
    }
}

pub struct CursorAgent;

impl AgentCliSpec for CursorAgent {
    fn ids(&self) -> &'static [&'static str] {
        &["cursor-agent"]
    }

    fn enrich_startup(&self, invoked_as: &str, user_args: &[String]) -> Vec<String> {
        debug_assert_eq!(invoked_as, "cursor-agent");
        enrich_agent_binary(user_args)
    }

    fn proxy_env_flags(&self, _invoked_as: &str) -> ProxyEnvFlags {
        ProxyEnvFlags::default()
    }
}
