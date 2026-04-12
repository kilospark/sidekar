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
fn enrich_opencode_resume_and_management_skip_starter() {
    assert_eq!(enrich("opencode", &["--continue"]), vec!["--continue"]);
    assert_eq!(enrich("opencode", &["-c"]), vec!["-c"]);
    assert_eq!(
        enrich("opencode", &["--session", "abc"]),
        vec!["--session", "abc"]
    );
    assert_eq!(enrich("opencode", &["run"]), vec!["run"]);
    assert_eq!(enrich("opencode", &["run", "-c"]), vec!["run", "-c"]);
    assert_eq!(enrich("opencode", &["session"]), vec!["session"]);
    assert_eq!(
        enrich("opencode", &["session", "list"]),
        vec!["session", "list"]
    );
    assert_eq!(enrich("opencode", &["models"]), vec!["models"]);
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
fn enrich_cursor_resume_and_picker_paths_skip_starter() {
    assert_eq!(enrich("agent", &["--resume"]), vec!["--resume"]);
    assert_eq!(enrich("agent", &["--continue"]), vec!["--continue"]);
    assert_eq!(enrich("agent", &["--cloud"]), vec!["--cloud"]);
    assert_eq!(enrich("agent", &["-c"]), vec!["-c"]);
    assert_eq!(
        enrich("agent", &["--resume", "chat-id"]),
        vec!["--resume", "chat-id"]
    );
    assert_eq!(enrich("agent", &["resume"]), vec!["resume"]);
    assert_eq!(enrich("agent", &["ls"]), vec!["ls"]);
    assert_eq!(enrich("cursor-agent", &["--resume"]), vec!["--resume"]);
    assert_eq!(
        enrich("cursor", &["agent", "--resume"]),
        vec!["agent", "--resume"]
    );
    assert_eq!(
        enrich("cursor", &["agent", "--continue"]),
        vec!["agent", "--continue"]
    );
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
fn enrich_claude_resume_paths_skip_starter() {
    assert_eq!(enrich("claude", &["--resume"]), vec!["--resume"]);
    assert_eq!(enrich("claude", &["-r"]), vec!["-r"]);
    assert_eq!(
        enrich("claude", &["--resume", "session-id"]),
        vec!["--resume", "session-id"]
    );
    assert_eq!(
        enrich("claude", &["--resume=session-id"]),
        vec!["--resume=session-id"]
    );
    assert_eq!(enrich("claude", &["--continue"]), vec!["--continue"]);
    assert_eq!(enrich("claude", &["-c"]), vec!["-c"]);
    assert_eq!(enrich("claude", &["--from-pr"]), vec!["--from-pr"]);
}

#[test]
fn enrich_codex_resume_subcommand_skips_starter() {
    assert_eq!(enrich("codex", &["resume"]), vec!["resume"]);
    assert_eq!(
        enrich("codex", &["--model", "gpt-5.4", "resume"]),
        vec!["--model", "gpt-5.4", "resume"]
    );
    assert_eq!(
        enrich("codex", &["resume", "--last"]),
        vec!["resume", "--last"]
    );
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
fn enrich_gemini_resume_and_session_management_skip_starter() {
    assert_eq!(
        enrich("gemini", &["--resume", "latest"]),
        vec!["--resume", "latest"]
    );
    assert_eq!(enrich("gemini", &["-r", "5"]), vec!["-r", "5"]);
    assert_eq!(
        enrich("gemini", &["--list-sessions"]),
        vec!["--list-sessions"]
    );
    assert_eq!(
        enrich("gemini", &["--delete-session", "3"]),
        vec!["--delete-session", "3"]
    );
    assert_eq!(
        enrich("gemini", &["--list-extensions"]),
        vec!["--list-extensions"]
    );
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
fn enrich_pi_resume_and_management_skip_starter() {
    assert_eq!(enrich("pi", &["--resume"]), vec!["--resume"]);
    assert_eq!(enrich("pi", &["-r"]), vec!["-r"]);
    assert_eq!(enrich("pi", &["--continue"]), vec!["--continue"]);
    assert_eq!(
        enrich("pi", &["--session", "session.jsonl"]),
        vec!["--session", "session.jsonl"]
    );
    assert_eq!(
        enrich("pi", &["install", "source"]),
        vec!["install", "source"]
    );
    assert_eq!(enrich("pi", &["list"]), vec!["list"]);
    assert_eq!(
        enrich("pi", &["--list-models", "sonnet"]),
        vec!["--list-models", "sonnet"]
    );
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
