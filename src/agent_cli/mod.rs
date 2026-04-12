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

/// Starter prompt passed via each tool’s native "initial prompt" mechanism.
pub const STARTUP_INJECT: &str = "never guess or assume. ask if unclear. no sycophancy. think critically. when working on a problem, do not take shortcuts or look for quickfixes. find the root cause. load sidekar skill.\noutput rules: terse, technical, no fluff. all substance stays, only filler dies. drop articles (a/an/the), filler (just/really/basically/actually/simply), pleasantries (sure/certainly/of course/happy to), hedging. fragments OK. short synonyms. technical terms exact. code blocks unchanged. errors quoted exact. pattern: [thing] [action] [reason]. [next step]. lead with the answer, not the reasoning. do not drift verbose over long conversations. code output, commits, file contents: write normally, not compressed. exception: use full clear prose for security warnings, irreversible action confirmations, and multi-step sequences where terse fragments risk misread. resume terse after.";

/// Registry spec: one type per agent **family** (or single binary). No default `proxy_env_flags`.
pub trait AgentCliSpec: Send + Sync {
    fn ids(&self) -> &'static [&'static str];
    fn enrich_startup(&self, invoked_as: &str, args: &[String]) -> Vec<String>;
    fn proxy_env_flags(&self, invoked_as: &str) -> ProxyEnvFlags;
}

fn skip_option_arg(args: &[String], i: usize, value_flags: &[&str]) -> usize {
    let arg = args[i].as_str();
    if !arg.starts_with('-') || arg == "-" {
        return i;
    }
    if arg.contains('=') {
        return i + 1;
    }
    if value_flags.contains(&arg) && i + 1 < args.len() {
        return i + 2;
    }
    i + 1
}

fn has_positional(args: &[String], value_flags: &[&str]) -> bool {
    let mut i = 0usize;
    while i < args.len() {
        if args[i].starts_with('-') && args[i] != "-" {
            i = skip_option_arg(args, i, value_flags);
        } else {
            return true;
        }
    }
    false
}

fn enrich_with_startup_prompt(args: &[String], value_flags: &[&str]) -> Vec<String> {
    let has_positional = has_positional(args, value_flags);
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
        enrich_with_startup_prompt(
            args,
            &[
                "--add-dir",
                "--agent",
                "--agents",
                "--allowedTools",
                "--allowed-tools",
                "--append-system-prompt",
                "--betas",
                "-d",
                "--debug",
                "--debug-file",
                "--disallowedTools",
                "--disallowed-tools",
                "--effort",
                "--fallback-model",
                "--file",
                "--from-pr",
                "--json-schema",
                "--max-budget-usd",
                "--mcp-config",
                "--model",
                "-m",
                "-n",
                "--name",
                "--output-format",
                "--permission-mode",
                "--plugin-dir",
                "-r",
                "--resume",
                "--remote-control-session-name-prefix",
                "--session-id",
                "--setting-sources",
                "--settings",
                "--system-prompt",
                "--tools",
                "-w",
                "--worktree",
            ],
        )
    }

    fn proxy_env_flags(&self, _invoked_as: &str) -> ProxyEnvFlags {
        // Claude Code is a Node client; global `fetch()` needs NODE_USE_ENV_PROXY=1
        // to honor HTTPS_PROXY, which is what routes it through sidekar's CONNECT MITM.
        ProxyEnvFlags {
            node_use_env_proxy: true,
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
        enrich_with_startup_prompt(
            args,
            &[
                "-a",
                "--add-dir",
                "--ask-for-approval",
                "-c",
                "--config",
                "-C",
                "--cd",
                "--disable",
                "--enable",
                "-i",
                "--image",
                "--local-provider",
                "-m",
                "--model",
                "-p",
                "--profile",
                "--remote",
                "--remote-auth-token-env",
                "-s",
                "--sandbox",
            ],
        )
    }

    fn proxy_env_flags(&self, _invoked_as: &str) -> ProxyEnvFlags {
        // Codex ChatGPT-subscription mode talks to chatgpt.com/backend-api/codex.
        // Setting OPENAI_BASE_URL would force codex into its API-key provider, so
        // we rely on HTTPS_PROXY + CA trust and let codex keep its native upstream.
        ProxyEnvFlags {
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
        let has_positional = has_positional(
            user_args,
            &[
                "-m",
                "--model",
                "-p",
                "--prompt",
                "-i",
                "--prompt-interactive",
                "-w",
                "--worktree",
                "--approval-mode",
                "--policy",
                "--admin-policy",
                "--allowed-mcp-server-names",
                "--allowed-tools",
                "-e",
                "--extensions",
                "-r",
                "--resume",
                "--delete-session",
                "--include-directories",
                "-o",
                "--output-format",
            ],
        );
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
        .find(|s| s.ids().contains(&invoked_as))
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
mod tests;
