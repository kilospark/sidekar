use super::*;

fn enrich(agent: &str, args: &[&str]) -> Vec<String> {
    let v: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    enrich_startup(agent, &v)
}

#[test]
fn enrich_opencode_prepends_prompt_before_project() {
    let out = enrich("opencode", &["."]);
    assert_eq!(out[0], "--prompt");
    assert_eq!(out[1], startup_inject());
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
    assert_eq!(out, vec!["agent", startup_inject()]);
}

#[test]
fn enrich_cursor_empty_inserts_agent_and_starter() {
    let out = enrich("cursor", &[]);
    assert_eq!(out, vec!["agent", startup_inject()]);
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
    assert_eq!(out, vec![startup_inject()]);
}

#[test]
fn enrich_cursor_agent_binary_matches_agent() {
    assert_eq!(enrich("cursor-agent", &[]), vec![startup_inject()]);
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
    assert_eq!(out, vec!["--model", "x", startup_inject()]);
}

#[test]
fn enrich_claude_codex_trailing_prompt_unchanged() {
    assert_eq!(enrich("claude", &[]), vec![startup_inject()]);
    assert_eq!(enrich("claude", &["hi"]), vec!["hi"]);
    assert_eq!(enrich("codex", &[]), vec![startup_inject()]);
}

#[test]
fn enrich_claude_codex_skip_option_values_before_injecting() {
    assert_eq!(
        enrich("claude", &["--model", "sonnet"]),
        vec!["--model", "sonnet", startup_inject()]
    );
    assert_eq!(
        enrich("codex", &["--model", "gpt-5.4"]),
        vec!["--model", "gpt-5.4", startup_inject()]
    );
}

#[test]
fn enrich_copilot_uses_dash_i() {
    assert_eq!(enrich("copilot", &[]), vec!["-i", startup_inject()]);
    assert_eq!(
        enrich("copilot", &["--model", "gpt-5.2"]),
        vec!["--model", "gpt-5.2", "-i", startup_inject()]
    );
}

#[test]
fn enrich_copilot_resume_headless_and_management_skip_starter() {
    assert_eq!(enrich("copilot", &["--continue"]), vec!["--continue"]);
    assert_eq!(
        enrich("copilot", &["--resume=session-id"]),
        vec!["--resume=session-id"]
    );
    assert_eq!(
        enrich("copilot", &["--prompt", "hello"]),
        vec!["--prompt", "hello"]
    );
    assert_eq!(enrich("copilot", &["login"]), vec!["login"]);
    assert_eq!(enrich("copilot", &["update"]), vec!["update"]);
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
    assert_eq!(out, vec!["-i", startup_inject()]);
}

#[test]
fn enrich_gemini_skip_option_values_before_injecting() {
    let out = enrich("gemini", &["--model", "gemini-2.5-pro"]);
    assert_eq!(
        out,
        vec!["--model", "gemini-2.5-pro", "-i", startup_inject()]
    );
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
    assert_eq!(out[1], startup_inject());
    assert_eq!(out.len(), 2);
}

#[test]
fn enrich_pi_skips_duplicate_starter_arg() {
    let out = enrich("pi", &[startup_inject()]);
    assert_eq!(out, vec![startup_inject()]);
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
fn enrich_grok_empty_gets_starter() {
    assert_eq!(enrich("grok", &[]), vec![startup_inject()]);
}

#[test]
fn enrich_grok_user_prompt_unchanged() {
    assert_eq!(enrich("grok", &["fix the bug"]), vec!["fix the bug"]);
}

#[test]
fn enrich_grok_headless_single_skips_starter() {
    assert_eq!(enrich("grok", &["-p", "hello"]), vec!["-p", "hello"]);
    assert_eq!(
        enrich("grok", &["--single", "hello"]),
        vec!["--single", "hello"]
    );
}

#[test]
fn enrich_grok_skip_option_values_before_injecting() {
    assert_eq!(
        enrich("grok", &["--model", "grok-build-0.1"]),
        vec!["--model", "grok-build-0.1", startup_inject()]
    );
}

#[test]
fn enrich_grok_resume_and_continue_skip_starter() {
    assert_eq!(enrich("grok", &["--continue"]), vec!["--continue"]);
    assert_eq!(enrich("grok", &["-c"]), vec!["-c"]);
    assert_eq!(enrich("grok", &["--resume"]), vec!["--resume"]);
    assert_eq!(enrich("grok", &["-r"]), vec!["-r"]);
    assert_eq!(
        enrich("grok", &["--resume", "session-id"]),
        vec!["--resume", "session-id"]
    );
    assert_eq!(
        enrich("grok", &["--prompt-file", "/tmp/p.txt"]),
        vec!["--prompt-file", "/tmp/p.txt"]
    );
}

#[test]
fn enrich_grok_management_subcommands_skip_starter() {
    assert_eq!(enrich("grok", &["login"]), vec!["login"]);
    assert_eq!(enrich("grok", &["models"]), vec!["models"]);
    assert_eq!(enrich("grok", &["sessions"]), vec!["sessions"]);
    assert_eq!(enrich("grok", &["update"]), vec!["update"]);
}

#[test]
fn is_pty_agent_matches_registry() {
    assert!(is_pty_agent("claude"));
    assert!(is_pty_agent("copilot"));
    assert!(is_pty_agent("grok"));
    assert!(is_pty_agent("pi"));
    assert!(!is_pty_agent("aider"));
    assert!(!is_pty_agent("goose"));
    assert!(!is_pty_agent("not-an-agent"));
}

#[test]
fn yolo_flags_are_agent_specific() {
    assert_eq!(yolo_flags("claude"), &["--dangerously-skip-permissions"]);
    assert_eq!(
        yolo_flags("codex"),
        &["--dangerously-bypass-approvals-and-sandbox"]
    );
    assert_eq!(yolo_flags("cursor-agent"), &["--force"]);
    assert_eq!(yolo_flags("gemini"), &["--yolo"]);
    assert_eq!(yolo_flags("copilot"), &["--allow-all"]);
    assert_eq!(
        yolo_flags("grok"),
        &["--permission-mode", "bypassPermissions"]
    );
}

#[test]
fn agents_without_an_unattended_mode_report_none() {
    // opencode's --auto exists only on `opencode run`, and pi has no permission
    // gate at all. Claiming a flag here would make spawn fail on first use.
    assert!(yolo_flags("opencode").is_empty());
    assert!(yolo_flags("pi").is_empty());
    assert!(!supports_yolo("opencode"));
    assert!(!supports_yolo("pi"));
    assert!(yolo_flags("not-an-agent").is_empty());
}

#[test]
fn apply_yolo_prepends_and_does_not_duplicate() {
    let none: Vec<String> = vec![];
    assert_eq!(
        apply_yolo("claude", &none),
        vec!["--dangerously-skip-permissions"]
    );

    // Ahead of the prompt: several CLIs read the first bare word as the task.
    let with_task = vec!["review the diff".to_string()];
    assert_eq!(
        apply_yolo("claude", &with_task),
        vec!["--dangerously-skip-permissions", "review the diff"]
    );

    // Already asked for: leave it alone.
    let explicit = vec![
        "--dangerously-skip-permissions".to_string(),
        "task".to_string(),
    ];
    assert_eq!(apply_yolo("claude", &explicit), explicit);

    // Nothing to add for an agent that has no unattended mode.
    let args = vec!["task".to_string()];
    assert_eq!(apply_yolo("opencode", &args), args);
}
