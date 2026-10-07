use super::*;

#[test]
fn ctrl_close_bracket_detaches_and_keeps_what_was_typed_before_it() {
    let (sent, detach) = filter_detach(b"ls\r\x1dafter".to_vec());
    assert!(detach);
    assert_eq!(
        sent, b"ls\r",
        "only what precedes the detach key reaches the session"
    );

    let (sent, detach) = filter_detach(b"plain input".to_vec());
    assert!(!detach);
    assert_eq!(sent, b"plain input");
}
