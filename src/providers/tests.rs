use super::{Provider, SseDecoder, openai_chat_completions_url, openai_models_url};

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

#[test]
fn sse_decoder_amortizes_many_small_events() {
    // Regression guard: before the read-cursor refactor, `next_event`
    // did `buffer = buffer[end..].to_string()` per call, making a
    // long stream of small frames O(n²) in frame count. This test
    // feeds 5000 small frames and asserts we can drain them all
    // quickly AND that the buffer doesn't retain everything.
    let mut decoder = SseDecoder::new();
    let mut expected = 0usize;
    for i in 0..5000 {
        decoder.push_chunk(format!("data: {{\"n\":{i}}}\n\n").as_bytes());
        // Drain as we go — mirrors real provider streaming where
        // next_event is called as soon as bytes arrive.
        while let Some(ev) = decoder.next_event() {
            assert!(ev.data.starts_with("{\"n\":"));
            expected += 1;
        }
    }
    assert_eq!(expected, 5000);
    // After full drain, the buffer should be small, not holding the
    // full concatenated 5000-frame history.
    assert!(
        decoder.buffer.len() < 8 * 1024,
        "expected drained buffer to be small, got {} bytes",
        decoder.buffer.len()
    );
    assert_eq!(decoder.read_pos, 0, "cursor should reset after compact");
}

#[test]
fn sse_decoder_handles_frame_split_across_chunks() {
    // SSE frames frequently arrive split across TCP/TLS chunks. The
    // decoder must hold partial frames until the "\n\n" terminator
    // is seen.
    let mut decoder = SseDecoder::new();
    decoder.push_chunk(b"data: {\"par");
    assert!(decoder.next_event().is_none(), "incomplete frame");
    decoder.push_chunk(b"t\":1}\n");
    assert!(
        decoder.next_event().is_none(),
        "single LF is not the terminator"
    );
    decoder.push_chunk(b"\n");
    let ev = decoder.next_event().expect("now complete");
    assert_eq!(ev.data, "{\"part\":1}");
}

#[test]
fn openai_compat_urls_accept_root_or_v1_or_full_endpoint() {
    assert_eq!(
        openai_chat_completions_url("https://api.x.ai"),
        "https://api.x.ai/v1/chat/completions"
    );
    assert_eq!(
        openai_chat_completions_url("https://api.x.ai/v1"),
        "https://api.x.ai/v1/chat/completions"
    );
    assert_eq!(
        openai_chat_completions_url("https://api.x.ai/v1/chat/completions"),
        "https://api.x.ai/v1/chat/completions"
    );
    assert_eq!(
        openai_models_url("https://api.x.ai/v1/chat/completions"),
        "https://api.x.ai/v1/models"
    );
}

#[test]
fn openai_compat_provider_type_is_preserved() {
    let grok = Provider::grok("key".to_string(), None);
    let compat = Provider::openai_compat(
        "key".to_string(),
        "http://localhost:11434/v1".to_string(),
        "local".to_string(),
        None,
    );

    assert_eq!(grok.provider_type(), "grok");
    assert_eq!(compat.provider_type(), "oac");
}
