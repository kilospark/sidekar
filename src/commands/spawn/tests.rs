use super::*;

#[test]
fn pid_is_read_from_every_pane_prefix() {
    assert_eq!(pid_of("pty-4242"), Some(4242));
    assert_eq!(pid_of("repl-7"), Some(7));
    assert_eq!(pid_of("cli-19"), Some(19));
}

#[test]
fn a_pane_without_a_pid_is_not_guessed_at() {
    assert_eq!(pid_of("mcp-abc"), None);
    assert_eq!(pid_of("pty-"), None);
    assert_eq!(pid_of(""), None);
    assert_eq!(pid_of("4242"), None);
}

#[test]
fn the_current_process_reads_as_alive() {
    assert!(is_alive(std::process::id() as i32));
    assert!(!is_alive(i32::MAX - 1));
}
