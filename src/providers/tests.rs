use super::SseDecoder;

#[test]
fn sse_decoder_parses_named_events_with_crlf_chunks() {
    let mut decoder = SseDecoder::new();
    decoder.push_chunk(b"event: message_start\r\ndata: {\"ok\":1}\r\n\r\n");

    let event = decoder.next_event().expect("expected SSE event");
    assert_eq!(event.event_type.as_deref(), Some("message_start"));
    assert_eq!(event.data, "{\"ok\":1}");
}

#[test]
fn sse_decoder_ignores_done_and_collects_data_only_events() {
    let mut decoder = SseDecoder::new();
    decoder.push_chunk(b"data: [DONE]\n\ndata: {\"type\":\"response.created\"}\n\n");

    let event = decoder.next_event().expect("expected SSE event");
    assert_eq!(event.event_type, None);
    assert_eq!(event.data, "{\"type\":\"response.created\"}");
    assert!(decoder.next_event().is_none());
}
