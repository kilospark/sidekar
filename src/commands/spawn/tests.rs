use super::*;

fn running(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, by)| (name.to_string(), by.to_string()))
        .collect()
}

#[test]
fn a_spawner_at_its_limit_is_refused_and_told_what_is_running() {
    let up = running(&[
        ("codex-a", "lead"),
        ("codex-b", "lead"),
        ("gemini-c", "other"),
    ]);
    let refusal = spawn_limit_refusal(&up, "lead", 2, 15).expect("lead is at 2");
    assert!(
        refusal.contains("lead already has 2 spawned agents running"),
        "{refusal}"
    );
    assert!(refusal.contains("codex-a, codex-b"), "{refusal}");
    assert!(
        !refusal.contains("gemini-c"),
        "only lead's own are named: {refusal}"
    );
    assert!(refusal.contains("sidekar stop <name>"), "{refusal}");
    assert!(
        refusal.contains("sidekar config set max_spawned_per_agent"),
        "{refusal}"
    );

    assert_eq!(
        spawn_limit_refusal(&up, "other", 2, 15),
        None,
        "other has one"
    );
}

#[test]
fn the_overall_limit_counts_every_spawner() {
    let up = running(&[("a", "x"), ("b", "y"), ("c", "z")]);
    let refusal = spawn_limit_refusal(&up, "w", 5, 3).expect("3 running, limit 3");
    assert!(
        refusal.contains("3 spawned agents are already running"),
        "{refusal}"
    );
    assert!(refusal.contains("max_spawned is 3"), "{refusal}");
    assert_eq!(spawn_limit_refusal(&up, "w", 5, 4), None);
}

#[test]
fn a_limit_of_zero_is_no_limit() {
    let up = running(&[("a", "x"), ("b", "x")]);
    assert_eq!(spawn_limit_refusal(&up, "x", 0, 0), None);
}

#[test]
fn the_wrapper_flags_reach_the_spawned_agent() {
    let wrapper = WrapperFlags {
        yolo: true,
        relay: Some(true),
        proxy: Some(false),
    };
    assert_eq!(
        child_argv("claude", &wrapper, Some("opus"), Some("Review the diff.")),
        [
            "claude",
            "--yolo",
            "--relay",
            "--no-proxy",
            "--model",
            "opus",
            "Review the diff."
        ]
    );

    // Unset, they are left to the agent's wrapper and its settings.
    let wrapper = WrapperFlags {
        yolo: false,
        relay: None,
        proxy: None,
    };
    assert_eq!(child_argv("codex", &wrapper, None, None), ["codex"]);
}

#[test]
fn the_footer_is_a_command_bus_send_parses() {
    // `bus send` reads only the `--reply-to=<id>` spelling; a footer that
    // taught `--reply-to <id>` would be followed and silently unlinked.
    let task = with_reply_footer("Review the diff.", "cli-app-1", "a1b2c3d4-0001");
    assert!(task.starts_with("Review the diff.\n\n"));
    assert!(task.contains("sidekar bus send cli-app-1 \"<your answer>\" --reply-to=a1b2c3d4-0001"));
}
