use super::*;

#[test]
fn tunnel_input_tracking_marks_pending_line() {
    let state = crate::poller::UserInputState::new();
    let mut line = Vec::new();

    track_user_input_chunk(&state, &mut line, b"abc");
    assert!(state.has_pending_line());
    assert_eq!(line, b"abc");
    assert_eq!(
        state.current_activity_state(),
        crate::activity::ActivityState::UserTyping
    );

    track_user_input_chunk(&state, &mut line, b"\n");
    assert!(!state.has_pending_line());
    assert!(line.is_empty());
}

#[test]
fn tunnel_escape_input_counts_as_activity_without_line_text() {
    let state = crate::poller::UserInputState::new();
    let mut line = Vec::new();

    track_user_input_chunk(&state, &mut line, b"\x1b[A");
    assert!(line.is_empty());
    assert!(!state.has_pending_line());
    assert_eq!(
        state.current_activity_state(),
        crate::activity::ActivityState::UserTyping
    );
}
