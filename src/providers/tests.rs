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
    let grok = Provider::grok("key".to_string());
    let compat = Provider::openai_compat(
        "key".to_string(),
        "http://localhost:11434/v1".to_string(),
        "local".to_string(),
    );

    assert_eq!(grok.provider_type(), "grok");
    assert_eq!(compat.provider_type(), "openai-compatible");
}
