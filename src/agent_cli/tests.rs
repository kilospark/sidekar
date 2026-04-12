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
fn enrich_claude_codex_skip_option_values_before_injecting() {
    assert_eq!(
        enrich("claude", &["--model", "sonnet"]),
        vec!["--model", "sonnet", STARTUP_INJECT]
    );
    assert_eq!(
        enrich("codex", &["--model", "gpt-5.4"]),
        vec!["--model", "gpt-5.4", STARTUP_INJECT]
    );
}

#[test]
fn enrich_claude_print_prompt_is_not_treated_as_option_value() {
    assert_eq!(enrich("claude", &["-p", "hello"]), vec!["-p", "hello"]);
}

#[test]
fn enrich_gemini_uses_dash_i() {
    let out = enrich("gemini", &[]);
    assert_eq!(out, vec!["-i", STARTUP_INJECT]);
}

#[test]
fn enrich_gemini_skip_option_values_before_injecting() {
    let out = enrich("gemini", &["--model", "gemini-2.5-pro"]);
    assert_eq!(out, vec!["--model", "gemini-2.5-pro", "-i", STARTUP_INJECT]);
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
