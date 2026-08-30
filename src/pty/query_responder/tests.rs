use super::*;

#[test]
fn answers_bare_and_zero_da1() {
    let mut responder = QueryResponder::new();
    assert_eq!(responder.feed(b"\x1b[c").unwrap(), DA1_RESPONSE);
    assert_eq!(responder.feed(b"\x1b[0c").unwrap(), DA1_RESPONSE);
}

#[test]
fn ignores_da1_response_form() {
    let mut responder = QueryResponder::new();
    // What a terminal sends back is private-prefixed; it must not loop.
    assert!(responder.feed(b"\x1b[?62;4;22c").is_none());
}

#[test]
fn answers_device_status_and_cursor_position() {
    let mut responder = QueryResponder::new();
    assert_eq!(responder.feed(b"\x1b[5n").unwrap(), DSR_OK);
    assert_eq!(responder.feed(b"\x1b[6n").unwrap(), CURSOR_POSITION);
    assert_eq!(
        responder.feed(b"\x1b[?6n").unwrap(),
        CURSOR_POSITION_PRIVATE
    );
}

#[test]
fn answers_osc_color_queries() {
    let mut responder = QueryResponder::new();
    let reply = responder.feed(b"\x1b]11;?\x07").unwrap();
    assert_eq!(reply, b"\x1b]11;rgb:0000/0000/0000\x1b\\");
}

#[test]
fn ignores_osc_color_sets_and_titles() {
    let mut responder = QueryResponder::new();
    assert!(responder.feed(b"\x1b]11;rgb:1111/2222/3333\x07").is_none());
    assert!(responder.feed(b"\x1b]0;my title\x07").is_none());
}

#[test]
fn batches_multiple_queries_in_one_chunk() {
    let mut responder = QueryResponder::new();
    let reply = responder.feed(b"\x1b[c hello \x1b[5n").unwrap();
    let mut expected = DA1_RESPONSE.to_vec();
    expected.extend_from_slice(DSR_OK);
    assert_eq!(reply, expected);
}

#[test]
fn carries_split_query_across_chunks() {
    let mut responder = QueryResponder::new();
    assert!(responder.feed(b"prompt \x1b[").is_none());
    assert_eq!(responder.feed(b"6n").unwrap(), CURSOR_POSITION);
}

#[test]
fn carries_split_osc_across_chunks() {
    let mut responder = QueryResponder::new();
    assert!(responder.feed(b"\x1b]10;").is_none());
    let reply = responder.feed(b"?\x07").unwrap();
    assert_eq!(reply, b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\");
}

#[test]
fn plain_output_produces_no_replies() {
    let mut responder = QueryResponder::new();
    assert!(responder.feed(b"just some agent text\r\n").is_none());
    assert!(responder.feed(b"\x1b[1;32mcolored\x1b[0m").is_none());
}

#[test]
fn unterminated_tail_is_bounded() {
    let mut responder = QueryResponder::new();
    let long: Vec<u8> = std::iter::once(0x1b)
        .chain(std::iter::once(b'['))
        .chain(std::iter::repeat_n(b'1', MAX_PARSE_TAIL * 2))
        .collect();
    responder.feed(&long);
    assert!(responder.tail.len() <= MAX_PARSE_TAIL);
}
