//! Child-process environment when `SIDEKAR_PROXY` is enabled.
//!
//! Every PTY-wrapped agent gets a **universal** block: HTTP(S) proxy pointing at
//! the local sidecar MITM listener plus several CA-path variables so Node, Python,
//! curl, git, and pip trust the ephemeral CA.
//!
//! On top of that, [`ProxyEnvFlags`] (from each [`super::AgentCliSpec`]) selects
//! optional reverse-proxy base URLs and Codex `config.toml` injection.

/// Optional layers on top of [`build_proxy_child_env`]'s universal MITM block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProxyEnvFlags {
    /// `ANTHROPIC_BASE_URL=http://127.0.0.1:<port>` — Claude Code reverse leg.
    pub anthropic_reverse: bool,
    /// `OPENAI_BASE_URL=http://127.0.0.1:<port>/v1` — OpenAI-shaped reverse leg.
    pub openai_reverse: bool,
    /// `CODEX_CA_CERTIFICATE=<pem path>` (Codex CLI also reads injected config.toml).
    pub codex_ca_certificate_env: bool,
    /// Run [`crate::proxy::inject_codex_ca`] before fork; pair with cleanup on exit.
    pub inject_codex_config_toml: bool,
    /// `NODE_USE_ENV_PROXY=1` — Node 18+ global `fetch()` ignores proxy env vars without this.
    pub node_use_env_proxy: bool,
}

/// Build `(env pairs, inject_codex_toml)` for the child. `ca_pem_path` should be
/// absolute or cwd-stable; it is copied into several `*_CA_*` / `SSL_*` variables.
pub fn build_proxy_child_env(
    invoked_as: &str,
    port: u16,
    ca_pem_path: &str,
) -> (Vec<(&'static str, String)>, bool) {
    let flags = super::spec_for(invoked_as)
        .map(|s| s.proxy_env_flags(invoked_as))
        .unwrap_or_default();

    let base = format!("http://127.0.0.1:{port}");
    let mut v: Vec<(&'static str, String)> = Vec::with_capacity(24);

    // --- Universal: CONNECT MITM + trust bundle (Node, Python requests, curl, git) ---
    v.push(("HTTPS_PROXY", base.clone()));
    v.push(("https_proxy", base.clone()));
    v.push(("HTTP_PROXY", base.clone()));
    v.push(("http_proxy", base.clone()));
    v.push(("ALL_PROXY", base.clone()));
    v.push(("all_proxy", base.clone()));
    v.push(("NO_PROXY", "127.0.0.1,localhost".into()));
    v.push(("no_proxy", "127.0.0.1,localhost".into()));

    for key in [
        "NODE_EXTRA_CA_CERTS",
        "SSL_CERT_FILE",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
        "GIT_SSL_CAINFO",
    ] {
        v.push((key, ca_pem_path.to_string()));
    }

    if flags.anthropic_reverse {
        v.push(("ANTHROPIC_BASE_URL", base.clone()));
    }
    if flags.openai_reverse {
        v.push(("OPENAI_BASE_URL", format!("{base}/v1")));
    }
    if flags.codex_ca_certificate_env {
        v.push(("CODEX_CA_CERTIFICATE", ca_pem_path.to_string()));
    }
    if flags.node_use_env_proxy {
        v.push(("NODE_USE_ENV_PROXY", "1".into()));
    }

    let inject_codex = flags.inject_codex_config_toml;
    (v, inject_codex)
}

#[cfg(test)]
mod tests;
