//! Per–command-line agent argv shaping for the PTY wrapper.
//!
//! Each [`AgentCliSpec`] registers one or more argv0 names (e.g. the Cursor family
//! covers `cursor`, `agent`, and `cursor-agent`).
//! [`AgentCliSpec::proxy_env_flags`] layers optional reverse-proxy URLs and Codex
//! hooks on top of the universal MITM env block in [`proxy_env::build_proxy_child_env`].

mod cursor_family;
mod proxy_env;

use cursor_family::CursorFamily;
use proxy_env::ProxyEnvFlags;
pub(crate) use proxy_env::build_proxy_child_env;

/// Starter prompt passed via each tool’s native “initial prompt” mechanism.
pub const STARTUP_INJECT: &str = "never guess or assume. ask if unclear. no sycophancy. think critically. when working on a problem, do not take shortcuts or look for quickfixes. find the root cause. load sidekar skill.";

/// Registry spec: one type per agent **family** (or single binary). No default `proxy_env_flags`.
pub trait AgentCliSpec: Send + Sync {
    fn ids(&self) -> &'static [&'static str];
    fn enrich_startup(&self, invoked_as: &str, args: &[String]) -> Vec<String>;
    fn proxy_env_flags(&self, invoked_as: &str) -> ProxyEnvFlags;
}

fn enrich_claude_codex_style(args: &[String]) -> Vec<String> {
    let has_positional = args.iter().any(|a| !a.starts_with('-'));
    let mut out = args.to_vec();
    if !has_positional {
        out.push(STARTUP_INJECT.to_string());
    }
    out
}

struct Claude;

impl AgentCliSpec for Claude {
    fn ids(&self) -> &'static [&'static str] {
        &["claude"]
    }

    fn enrich_startup(&self, invoked_as: &str, args: &[String]) -> Vec<String> {
        debug_assert_eq!(invoked_as, "claude");
        enrich_claude_codex_style(args)
    }

    fn proxy_env_flags(&self, _invoked_as: &str) -> ProxyEnvFlags {
        ProxyEnvFlags {
            anthropic_reverse: true,
            ..Default::default()
        }
    }
}

struct Codex;

impl AgentCliSpec for Codex {
    fn ids(&self) -> &'static [&'static str] {
        &["codex"]
    }

    fn enrich_startup(&self, invoked_as: &str, args: &[String]) -> Vec<String> {
        debug_assert_eq!(invoked_as, "codex");
        enrich_claude_codex_style(args)
    }

    fn proxy_env_flags(&self, _invoked_as: &str) -> ProxyEnvFlags {
        ProxyEnvFlags {
            openai_reverse: true,
            codex_ca_certificate_env: true,
            inject_codex_config_toml: true,
            ..Default::default()
        }
    }
}

struct Gemini;

impl AgentCliSpec for Gemini {
    fn ids(&self) -> &'static [&'static str] {
        &["gemini"]
    }

    fn enrich_startup(&self, invoked_as: &str, user_args: &[String]) -> Vec<String> {
        debug_assert_eq!(invoked_as, "gemini");
        let has_positional = user_args.iter().any(|a| !a.starts_with('-'));
        let has_flag = |flags: &[&str]| -> bool {
            user_args.iter().any(|a| {
                flags
                    .iter()
                    .any(|f| a == f || a.starts_with(&format!("{f}=")))
            })
        };
        let mut out = user_args.to_vec();
        if !has_positional && !has_flag(&["-i", "--prompt-interactive", "-p", "--prompt"]) {
            out.push("-i".into());
            out.push(STARTUP_INJECT.to_string());
        }
        out
    }

    fn proxy_env_flags(&self, _invoked_as: &str) -> ProxyEnvFlags {
        // Google Gemini CLI: MITM + CA trust; add provider-specific vars when documented.
        ProxyEnvFlags::default()
    }
}

struct OpenCode;

impl AgentCliSpec for OpenCode {
    fn ids(&self) -> &'static [&'static str] {
        &["opencode"]
    }

    fn enrich_startup(&self, invoked_as: &str, user_args: &[String]) -> Vec<String> {
        debug_assert_eq!(invoked_as, "opencode");
        let has_flag = |flags: &[&str]| -> bool {
            user_args.iter().any(|a| {
                flags
                    .iter()
                    .any(|f| a == f || a.starts_with(&format!("{f}=")))
            })
        };
        if !has_flag(&["--prompt"]) {
            let mut prefixed = Vec::with_capacity(user_args.len().saturating_add(2));
            prefixed.push("--prompt".into());
            prefixed.push(STARTUP_INJECT.to_string());
            prefixed.extend(user_args.iter().cloned());
            return prefixed;
        }
        user_args.to_vec()
    }

    fn proxy_env_flags(&self, _invoked_as: &str) -> ProxyEnvFlags {
        // Multi-provider TUI: MITM + CA only unless we add per-provider base URLs.
        ProxyEnvFlags::default()
    }
}

/// Pi: `--append-system-prompt` adds to the default system prompt (see pi coding-agent CLI).
fn enrich_pi_startup(user_args: &[String]) -> Vec<String> {
    if user_args.iter().any(|a| a.as_str() == STARTUP_INJECT) {
        return user_args.to_vec();
    }
    let mut out = Vec::with_capacity(user_args.len().saturating_add(2));
    out.push("--append-system-prompt".into());
    out.push(STARTUP_INJECT.to_string());
    out.extend(user_args.iter().cloned());
    out
}

struct Pi;

impl AgentCliSpec for Pi {
    fn ids(&self) -> &'static [&'static str] {
        &["pi"]
    }

    fn enrich_startup(&self, invoked_as: &str, user_args: &[String]) -> Vec<String> {
        debug_assert_eq!(invoked_as, "pi");
        enrich_pi_startup(user_args)
    }

    fn proxy_env_flags(&self, _invoked_as: &str) -> ProxyEnvFlags {
        ProxyEnvFlags::default()
    }
}

static CLAUDE: Claude = Claude;
static CODEX: Codex = Codex;
static CURSOR_FAMILY: CursorFamily = CursorFamily;
static GEMINI: Gemini = Gemini;
static OPENCODE: OpenCode = OpenCode;
static PI: Pi = Pi;

static REGISTRY: &[&dyn AgentCliSpec] = &[&CLAUDE, &CODEX, &CURSOR_FAMILY, &GEMINI, &OPENCODE, &PI];

pub(super) fn spec_for(invoked_as: &str) -> Option<&'static dyn AgentCliSpec> {
    REGISTRY
        .iter()
        .copied()
        .find(|s| s.ids().iter().any(|&id| id == invoked_as))
}

/// True when `sidekar <name> …` should PTY-wrap this binary.
pub fn is_pty_agent(name: &str) -> bool {
    spec_for(name).is_some()
}

/// Apply startup injection for `invoked_as` if the registry entry supports it.
pub fn enrich_startup(invoked_as: &str, args: &[String]) -> Vec<String> {
    spec_for(invoked_as)
        .map(|s| s.enrich_startup(invoked_as, args))
        .unwrap_or_else(|| args.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enrich(agent: &str, args: &[&str]) -> Vec<String> {
        let v: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        enrich_startup(agent, &v)
    }

    #[test]
    fn enrich_opencode_prepends_prompt_before_project() {
        let out = enrich("opencode", &["."]);
        assert_eq!(out[0], "--prompt");
        assert_eq!(out[1], STARTUP_INJECT);
        assert_eq!(out[2], ".");
    }

    #[test]
    fn enrich_opencode_skips_when_prompt_present() {
        let out = enrich("opencode", &["--prompt", "user", "."]);
        assert_eq!(out, vec!["--prompt", "user", "."]);
    }

    #[test]
    fn enrich_cursor_agent_tail_gets_starter() {
        let out = enrich("cursor", &["agent"]);
        assert_eq!(out, vec!["agent", STARTUP_INJECT]);
    }

    #[test]
    fn enrich_cursor_empty_inserts_agent_and_starter() {
        let out = enrich("cursor", &[]);
        assert_eq!(out, vec!["agent", STARTUP_INJECT]);
    }

    #[test]
    fn enrich_cursor_agent_with_user_prompt_skips_starter() {
        let out = enrich("cursor", &["agent", "ship", "it"]);
        assert_eq!(out, vec!["agent", "ship", "it"]);
    }

    #[test]
    fn enrich_cursor_agent_login_skips_starter() {
        let out = enrich("cursor", &["agent", "login"]);
        assert_eq!(out, vec!["agent", "login"]);
    }

    #[test]
    fn enrich_agent_binary_empty_gets_starter() {
        let out = enrich("agent", &[]);
        assert_eq!(out, vec![STARTUP_INJECT]);
    }

    #[test]
    fn enrich_cursor_agent_binary_matches_agent() {
        assert_eq!(enrich("cursor-agent", &[]), vec![STARTUP_INJECT]);
        assert_eq!(enrich("cursor-agent", &["login"]), vec!["login"]);
    }

    #[test]
    fn enrich_agent_login_skips_starter() {
        let out = enrich("agent", &["login"]);
        assert_eq!(out, vec!["login"]);
    }

    #[test]
    fn enrich_agent_flags_only_gets_starter() {
        let out = enrich("agent", &["--model", "x"]);
        assert_eq!(out, vec!["--model", "x", STARTUP_INJECT]);
    }

    #[test]
    fn enrich_claude_codex_trailing_prompt_unchanged() {
        assert_eq!(enrich("claude", &[]), vec![STARTUP_INJECT]);
        assert_eq!(enrich("claude", &["hi"]), vec!["hi"]);
        assert_eq!(enrich("codex", &[]), vec![STARTUP_INJECT]);
    }

    #[test]
    fn enrich_gemini_uses_dash_i() {
        let out = enrich("gemini", &[]);
        assert_eq!(out, vec!["-i", STARTUP_INJECT]);
    }

    #[test]
    fn enrich_pi_prepends_append_system_prompt() {
        let out = enrich("pi", &[]);
        assert_eq!(out[0], "--append-system-prompt");
        assert_eq!(out[1], STARTUP_INJECT);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn enrich_pi_skips_duplicate_starter_arg() {
        let out = enrich("pi", &[STARTUP_INJECT]);
        assert_eq!(out, vec![STARTUP_INJECT]);
    }

    #[test]
    fn unknown_binary_passes_args_through() {
        assert_eq!(enrich("not-an-agent", &["a"]), vec!["a"]);
    }

    #[test]
    fn is_pty_agent_matches_registry() {
        assert!(is_pty_agent("claude"));
        assert!(is_pty_agent("pi"));
        assert!(!is_pty_agent("aider"));
        assert!(!is_pty_agent("goose"));
        assert!(!is_pty_agent("not-an-agent"));
    }
}
