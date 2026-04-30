use super::{
    ContentBlock, MODEL_CATALOG_TIMEOUT_SECS, Provider, SseDecoder, catalog_http_client,
    is_retryable_error, openai_chat_completions_url,
    openai_compat_assistant_concat_reasoning_chunks, openai_compat_assistant_join_text,
    openai_models_url, openai_plain_text_before_first_tool_call, provider_models_list_client,
};

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
    // Custom endpoint with existing path (e.g. Vertex AI) — no /v1/ injected
    assert_eq!(
        openai_chat_completions_url(
            "https://aiplatform.googleapis.com/v1/projects/foo/locations/global/endpoints/openapi"
        ),
        "https://aiplatform.googleapis.com/v1/projects/foo/locations/global/endpoints/openapi/chat/completions"
    );
    assert_eq!(
        openai_models_url(
            "https://aiplatform.googleapis.com/v1/projects/foo/locations/global/endpoints/openapi"
        ),
        "https://aiplatform.googleapis.com/v1/projects/foo/locations/global/endpoints/openapi/models"
    );
    // Trailing slash stripped
    assert_eq!(
        openai_chat_completions_url(
            "https://aiplatform.googleapis.com/v1/projects/foo/locations/global/endpoints/openapi/"
        ),
        "https://aiplatform.googleapis.com/v1/projects/foo/locations/global/endpoints/openapi/chat/completions"
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

// ---- is_retryable_error classifier ------------------------------
//
// The message shapes below are verbatim from production failures.
// Do NOT lowercase the fixtures — the classifier must handle the
// real OS/reqwest capitalization. Regression for mid-stream retries
// not firing on `Connection reset by peer (os error 54)`.

fn err(msg: &str) -> anyhow::Error {
    anyhow::anyhow!(msg.to_string())
}

fn chained(outer: &str, inner: &str) -> anyhow::Error {
    err(inner).context(outer.to_string())
}

#[test]
fn retryable_connection_reset_capitalized() {
    // The exact string emitted by reqwest when the peer RSTs a live
    // SSE stream on macOS. Capital C — must match case-insensitively.
    let e = chained(
        "error reading SSE chunk",
        "error decoding response body: request or response body error: \
         error reading a body from connection: Connection reset by peer \
         (os error 54)",
    );
    assert!(is_retryable_error(&e), "got non-retryable: {e:#}");
}

#[test]
fn retryable_5xx_and_429() {
    assert!(is_retryable_error(&err("api error (500): foo")));
    assert!(is_retryable_error(&err("api error (502): foo")));
    assert!(is_retryable_error(&err("api error (503): foo")));
    assert!(is_retryable_error(&err("api error (504): foo")));
    assert!(is_retryable_error(&err("api error (529): overloaded")));
    assert!(is_retryable_error(&err("api error (429): rate limited")));
}

#[test]
fn retryable_transport_shapes() {
    assert!(is_retryable_error(&err("failed to connect to host")));
    assert!(is_retryable_error(&err("operation timed out")));
    assert!(is_retryable_error(&err("Broken pipe (os error 32)")));
    assert!(is_retryable_error(&err(
        "connection closed before message completed"
    )));
    assert!(is_retryable_error(&err(
        "connection error: incomplete message"
    )));
    assert!(is_retryable_error(&err(
        "unexpected EOF during chunk size line"
    )));
}

#[test]
fn not_retryable_4xx_client_errors() {
    assert!(!is_retryable_error(&err("api error (400): bad request")));
    assert!(!is_retryable_error(&err("api error (401): unauthorized")));
    assert!(!is_retryable_error(&err("api error (403): forbidden")));
    assert!(!is_retryable_error(&err("api error (404): not found")));
}

#[test]
fn catalog_http_clients_build_successfully() {
    assert!(catalog_http_client(MODEL_CATALOG_TIMEOUT_SECS).is_ok());
    assert!(provider_models_list_client(MODEL_CATALOG_TIMEOUT_SECS).is_some());
}

#[test]
fn openai_compat_assistant_text_helpers_split_text_and_reasoning() {
    let blocks = vec![
        ContentBlock::Text { text: "a".into() },
        ContentBlock::Reasoning { text: "r".into() },
        ContentBlock::Text { text: "b".into() },
    ];
    assert_eq!(openai_compat_assistant_join_text(&blocks), "a\nb");
    assert_eq!(
        openai_compat_assistant_concat_reasoning_chunks(&blocks),
        "r"
    );

    let mixed = vec![
        ContentBlock::Thinking {
            thinking: "t1".into(),
            signature: "".into(),
        },
        ContentBlock::Reasoning { text: "r1".into() },
    ];
    assert_eq!(
        openai_compat_assistant_concat_reasoning_chunks(&mixed),
        "t1r1"
    );
}

#[test]
fn plain_text_before_first_tool_stops_at_tool_call() {
    let blocks = vec![
        ContentBlock::Text {
            text: "step 1".into(),
        },
        ContentBlock::Reasoning {
            text: "ignored here".into(),
        },
        ContentBlock::ToolCall {
            id: "x".into(),
            name: "n".into(),
            arguments: serde_json::json!({}),
            thought_signature: None,
        },
        ContentBlock::Text {
            text: "after".into(),
        },
    ];
    assert_eq!(openai_plain_text_before_first_tool_call(&blocks), "step 1");
}
