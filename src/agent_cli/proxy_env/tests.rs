use super::*;

fn has(haystack: &[(&'static str, String)], key: &str) -> bool {
    haystack.iter().any(|(k, _)| *k == key)
}

fn val<'a>(haystack: &'a [(&'static str, String)], key: &str) -> &'a str {
    haystack
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.as_str())
        .unwrap_or("")
}

#[test]
fn claude_uses_connect_mitm_with_node_fetch_proxy_env() {
    let (env, inject) = build_proxy_child_env("claude", 9, "/tmp/ca.pem");
    assert!(!inject);
    // No reverse base URL — Claude Code keeps its native api.anthropic.com
    // upstream and CONNECT-tunnels through HTTPS_PROXY.
    assert!(!has(&env, "ANTHROPIC_BASE_URL"));
    assert!(!has(&env, "OPENAI_BASE_URL"));
    assert!(has(&env, "HTTPS_PROXY"));
    assert_eq!(val(&env, "NODE_USE_ENV_PROXY"), "1");
    assert_eq!(val(&env, "NODE_EXTRA_CA_CERTS"), "/tmp/ca.pem");
}

#[test]
fn codex_uses_connect_mitm_with_ca_injection() {
    let (env, inject) = build_proxy_child_env("codex", 9, "/tmp/ca.pem");
    // config.toml still gets the CA injected so codex trusts sidekar's cert.
    assert!(inject);
    // No reverse base URL — setting OPENAI_BASE_URL would force codex out of
    // ChatGPT-subscription mode into API-key mode.
    assert!(!has(&env, "ANTHROPIC_BASE_URL"));
    assert!(!has(&env, "OPENAI_BASE_URL"));
    assert!(has(&env, "HTTPS_PROXY"));
    assert_eq!(val(&env, "CODEX_CA_CERTIFICATE"), "/tmp/ca.pem");
}

#[test]
fn cursor_family_sets_node_use_env_proxy() {
    for agent in &["cursor", "agent", "cursor-agent"] {
        let (env, inject) = build_proxy_child_env(agent, 9, "/tmp/ca.pem");
        assert!(!inject);
        assert!(!has(&env, "ANTHROPIC_BASE_URL"));
        assert!(!has(&env, "OPENAI_BASE_URL"));
        assert_eq!(val(&env, "NODE_USE_ENV_PROXY"), "1");
        assert!(has(&env, "HTTPS_PROXY"));
    }
}

#[test]
fn gemini_mitm_only() {
    let (env, inject) = build_proxy_child_env("gemini", 9, "/x/ca.pem");
    assert!(!inject);
    assert!(!has(&env, "ANTHROPIC_BASE_URL"));
    assert!(!has(&env, "OPENAI_BASE_URL"));
    assert!(!has(&env, "NODE_USE_ENV_PROXY"));
    assert!(has(&env, "SSL_CERT_FILE"));
}
