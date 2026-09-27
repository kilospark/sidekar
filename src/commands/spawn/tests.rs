use super::*;

#[test]
fn the_footer_is_a_command_bus_send_parses() {
    // `bus send` reads only the `--reply-to=<id>` spelling; a footer that
    // taught `--reply-to <id>` would be followed and silently unlinked.
    let task = with_reply_footer("Review the diff.", "cli-app-1", "a1b2c3d4-0001");
    assert!(task.starts_with("Review the diff.\n\n"));
    assert!(task.contains("sidekar bus send cli-app-1 \"<your answer>\" --reply-to=a1b2c3d4-0001"));
}
