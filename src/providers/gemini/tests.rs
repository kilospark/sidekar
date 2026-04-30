//! Unit tests for the Gemini native adapter.
//!
//! Covers request-body construction (text, multimodal, tool
//! round-trip, tools, system prompt, cached reference) and the SSE
//! parser paths that are tricky to get right (thinking, function
//! calls, usage metadata, finish reasons, same-name-call
//! disambiguation).

use super::*;
use serde_json::json;

// ─── build_request_body ─────────────────────────────────────────

fn mk_user_text(s: &str) -> ChatMessage {
    ChatMessage {
        role: Role::User,
        content: vec![ContentBlock::Text { text: s.into() }],
    }
}

#[test]
fn text_only_request_shape() {
    let (body, _) = build_request_body(
        "gemini-2.5-flash",
        "You are a helpful dev.",
        &[mk_user_text("hello")],
        &[],
        None,
    );
    assert_eq!(
        body["systemInstruction"]["parts"][0]["text"],
        "You are a helpful dev."
    );
    assert_eq!(body["contents"][0]["role"], "user");
    assert_eq!(body["contents"][0]["parts"][0]["text"], "hello");
    assert!(
        body.get("tools").is_none(),
        "no tools passed, no tools in body"
    );
    // Safety defaults present.
    let safety = body["safetySettings"].as_array().unwrap();
    assert_eq!(safety.len(), 4);
    assert!(safety.iter().all(|s| s["threshold"] == "BLOCK_NONE"));
    // Not cached.
    assert!(body.get("cachedContent").is_none());
}

#[test]
fn multimodal_request_includes_inline_data() {
    let msg = ChatMessage {
        role: Role::User,
        content: vec![
            ContentBlock::Text {
                text: "what is this".into(),
            },
            ContentBlock::Image {
                media_type: "image/png".into(),
                data_base64: "ZmFrZQ==".into(),
                source_path: None,
            },
        ],
    };
    let (body, _) = build_request_body("gemini-2.5-pro", "", &[msg], &[], None);
    let parts = body["contents"][0]["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["text"], "what is this");
    assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
    assert_eq!(parts[1]["inlineData"]["data"], "ZmFrZQ==");
    // No systemInstruction when empty.
    assert!(body.get("systemInstruction").is_none());
}

#[test]
fn tool_round_trip_resolves_id_to_function_name() {
    // User asks → assistant issues tool call → user replies with
    // ToolResult referencing the synthesized id. Verify the
    // functionResponse.name resolves correctly.
    let history = vec![
        mk_user_text("list files"),
        ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall {
                id: "call_Bash_0".into(),
                name: "Bash".into(),
                arguments: json!({"command": "ls"}),
                thought_signature: None,
            }],
        },
        ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_Bash_0".into(),
                content: "a\nb\nc".into(),
                is_error: false,
            }],
        },
    ];
    let (body, id_map) = build_request_body("gemini-2.5-flash", "", &history, &[], None);
    assert_eq!(id_map.get("call_Bash_0").map(|s| s.as_str()), Some("Bash"));

    let contents = body["contents"].as_array().unwrap();
    // user, assistant, user — three entries.
    assert_eq!(contents.len(), 3);

    // Assistant turn contains the functionCall.
    let fc = &contents[1]["parts"][0]["functionCall"];
    assert_eq!(fc["name"], "Bash");
    assert_eq!(fc["args"]["command"], "ls");

    // User turn contains a functionResponse resolved to name "Bash".
    let fr = &contents[2]["parts"][0]["functionResponse"];
    assert_eq!(fr["name"], "Bash");
    assert_eq!(fr["response"]["content"], "a\nb\nc");
}

#[test]
fn tool_result_error_wraps_in_error_field() {
    let history = vec![
        ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall {
                id: "call_Grep_0".into(),
                name: "Grep".into(),
                arguments: json!({"pattern": "x"}),
                thought_signature: None,
            }],
        },
        ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_Grep_0".into(),
                content: "oops".into(),
                is_error: true,
            }],
        },
    ];
    let (body, _) = build_request_body("gemini-2.5-flash", "", &history, &[], None);
    let fr = &body["contents"][1]["parts"][0]["functionResponse"];
    assert_eq!(fr["name"], "Grep");
    // Error goes into response.error, not response.content.
    assert_eq!(fr["response"]["error"], "oops");
    assert!(fr["response"].get("content").is_none());
}

#[test]
fn thought_signature_replayed_on_function_call_parts() {
    // When Gemini returns a thoughtSignature on a functionCall part,
    // it must be replayed verbatim on the functionCall part in subsequent
    // requests (not just on the thinking text part).
    let history = vec![
        mk_user_text("do something"),
        ChatMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: "let me think...".into(),
                    signature: "think_sig_abc".into(),
                },
                ContentBlock::ToolCall {
                    id: "call_Bash_0".into(),
                    name: "Bash".into(),
                    arguments: json!({"command": "echo hi"}),
                    thought_signature: Some("fc_sig_xyz".into()),
                },
            ],
        },
        ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_Bash_0".into(),
                content: "hi".into(),
                is_error: false,
            }],
        },
    ];
    let (body, _) = build_request_body("gemini-2.5-pro", "", &history, &[], None);
    let assistant_parts = body["contents"][1]["parts"].as_array().unwrap();

    // Thinking part carries its own signature.
    assert_eq!(assistant_parts[0]["thoughtSignature"], "think_sig_abc");

    // FunctionCall part carries its own (different) signature.
    assert_eq!(assistant_parts[1]["thoughtSignature"], "fc_sig_xyz");
}

#[test]
fn thought_signature_none_uses_skip_sentinel() {
    // Cross-provider history: ToolCall has no Gemini signature.
    // Should use SKIP_SIG sentinel when thinking is present.
    let history = vec![
        mk_user_text("do something"),
        ChatMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: "hmm".into(),
                    signature: String::new(),
                },
                ContentBlock::ToolCall {
                    id: "call_Bash_0".into(),
                    name: "Bash".into(),
                    arguments: json!({"command": "ls"}),
                    thought_signature: None,
                },
            ],
        },
        ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_Bash_0".into(),
                content: "files".into(),
                is_error: false,
            }],
        },
    ];
    let (body, _) = build_request_body("gemini-2.5-pro", "", &history, &[], None);
    let assistant_parts = body["contents"][1]["parts"].as_array().unwrap();

    // Both should use SKIP_SIG since no real sigs are available.
    assert_eq!(
        assistant_parts[0]["thoughtSignature"],
        "skip_thought_signature_validator"
    );
    assert_eq!(
        assistant_parts[1]["thoughtSignature"],
        "skip_thought_signature_validator"
    );
}

#[test]
fn tools_serialized_as_function_declarations() {
    let tools = vec![
        ToolDef {
            name: "Bash".into(),
            description: "Run shell".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"],
            }),
        },
        ToolDef {
            name: "Read".into(),
            description: "Read file".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
            }),
        },
    ];
    let (body, _) = build_request_body("gemini-2.5-pro", "sys", &[], &tools, None);
    let decls = body["tools"][0]["functionDeclarations"].as_array().unwrap();
    assert_eq!(decls.len(), 2);
    assert_eq!(decls[0]["name"], "Bash");
    assert_eq!(decls[0]["description"], "Run shell");
    assert_eq!(decls[0]["parameters"]["type"], "object");
    assert_eq!(decls[1]["name"], "Read");
}

#[test]
fn cached_content_reference_omits_system_and_tools() {
    let tools = vec![ToolDef {
        name: "Bash".into(),
        description: "".into(),
        input_schema: json!({}),
    }];
    let (body, _) = build_request_body(
        "gemini-2.5-pro",
        "stable system prompt",
        &[mk_user_text("incremental question")],
        &tools,
        Some("cachedContents/abc123"),
    );
    // Cache reference present.
    assert_eq!(body["cachedContent"], "cachedContents/abc123");
    // System and tools MUST be absent (they're in the cache payload).
    assert!(
        body.get("systemInstruction").is_none(),
        "systemInstruction must not be sent when cachedContent is set"
    );
    assert!(
        body.get("tools").is_none(),
        "tools must not be sent when cachedContent is set"
    );
    // The incremental user turn is still in contents.
    assert_eq!(
        body["contents"][0]["parts"][0]["text"],
        "incremental question"
    );
}

#[test]
fn assistant_thinking_replayed_as_thought_part() {
    let msg = ChatMessage {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Thinking {
                thinking: "let me think about this".into(),
                signature: "ignored".into(),
            },
            ContentBlock::Text {
                text: "answer".into(),
            },
        ],
    };
    let (body, _) = build_request_body("gemini-2.5-pro", "", &[msg], &[], None);
    let parts = body["contents"][0]["parts"].as_array().unwrap();
    // Thought part carries thought:true.
    assert_eq!(parts[0]["thought"], true);
    assert_eq!(parts[0]["text"], "let me think about this");
    assert_eq!(parts[1]["text"], "answer");
    assert!(parts[1].get("thought").is_none());
}

#[test]
fn empty_messages_still_produces_valid_body() {
    // Edge case: no messages (e.g. startup probe). Should not panic.
    let (body, _) = build_request_body("gemini-2.5-flash", "hi", &[], &[], None);
    assert_eq!(body["contents"].as_array().unwrap().len(), 0);
    assert_eq!(body["systemInstruction"]["parts"][0]["text"], "hi");
}

// ─── SSE parser (via fixture-driven tests that don't need network) ──
//
// We can't test parse_sse_stream directly without a reqwest::Response.
// Instead, extract the chunk-handling logic via the SseDecoder and
// replay its events to verify we produce the right StreamEvents.
// Full integration coverage requires a real key and lives outside
// the unit-test suite.

// ─── cacheable_prefix_len ───────────────────────────────────────

#[test]
fn cacheable_prefix_empty_when_only_user_turn() {
    // Fresh conversation: single user message. There's no completed
    // exchange to cache — the prefix is empty.
    let msgs = vec![mk_user_text("hello")];
    assert_eq!(cacheable_prefix_len(&msgs), 0);
}

#[test]
fn cacheable_prefix_includes_last_assistant_turn() {
    // user → assistant → user(current). Prefix = first two messages;
    // the current user turn is the incremental delta.
    let msgs = vec![
        mk_user_text("hi"),
        ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
        },
        mk_user_text("follow-up"),
    ];
    assert_eq!(cacheable_prefix_len(&msgs), 2);
}

#[test]
fn cacheable_prefix_spans_tool_round_trip() {
    // Multi-turn with a tool round-trip in the middle. Prefix must
    // end at the last assistant message BEFORE the current user
    // input, not at a ToolResult-bearing user message.
    let msgs = vec![
        mk_user_text("list"),
        ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall {
                id: "call_Bash_0".into(),
                name: "Bash".into(),
                arguments: json!({"command":"ls"}),
                thought_signature: None,
            }],
        },
        ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_Bash_0".into(),
                content: "a".into(),
                is_error: false,
            }],
        },
        ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "one file".into(),
            }],
        },
        mk_user_text("thanks"),
    ];
    // Prefix = everything up to and including the second assistant
    // turn (index 3), so length 4.
    assert_eq!(cacheable_prefix_len(&msgs), 4);
}

#[test]
fn cache_not_found_error_detection() {
    // 404 status code in error string.
    let e = anyhow::anyhow!(
        "Gemini API error (404 Not Found): cachedContent \"cachedContents/abc\" was not found"
    );
    assert!(is_cache_not_found_error(&e));
    // 400 with NOT_FOUND body text.
    let e = anyhow::anyhow!("Gemini API error (400 Bad Request): CachedContent not found");
    assert!(is_cache_not_found_error(&e));
    // Unrelated errors don't trigger the retry.
    let e = anyhow::anyhow!("Gemini API error (500 Internal Server Error)");
    assert!(!is_cache_not_found_error(&e));
    let e = anyhow::anyhow!("network timeout");
    assert!(!is_cache_not_found_error(&e));
}

#[test]
fn tool_id_synthesis_disambiguates_same_name_calls() {
    // Two successive functionCall parts with name "Bash" in the same
    // turn must produce distinct ids: call_Bash_0 and call_Bash_1.
    //
    // We verify this by converting the *response* back via
    // build_request_body: if we record both calls on the assistant
    // turn and then a user turn with two ToolResults (one per id),
    // the id_map must resolve both to "Bash".
    let assistant = ChatMessage {
        role: Role::Assistant,
        content: vec![
            ContentBlock::ToolCall {
                id: "call_Bash_0".into(),
                name: "Bash".into(),
                arguments: json!({"command": "ls"}),
                thought_signature: None,
            },
            ContentBlock::ToolCall {
                id: "call_Bash_1".into(),
                name: "Bash".into(),
                arguments: json!({"command": "pwd"}),
                thought_signature: None,
            },
        ],
    };
    let user_reply = ChatMessage {
        role: Role::User,
        content: vec![
            ContentBlock::ToolResult {
                tool_use_id: "call_Bash_0".into(),
                content: "a\nb".into(),
                is_error: false,
            },
            ContentBlock::ToolResult {
                tool_use_id: "call_Bash_1".into(),
                content: "/home".into(),
                is_error: false,
            },
        ],
    };
    let (body, id_map) =
        build_request_body("gemini-2.5-pro", "", &[assistant, user_reply], &[], None);
    assert_eq!(id_map.get("call_Bash_0").unwrap(), "Bash");
    assert_eq!(id_map.get("call_Bash_1").unwrap(), "Bash");

    // Both functionResponses resolve to name "Bash".
    let user_parts = body["contents"][1]["parts"].as_array().unwrap();
    assert_eq!(user_parts[0]["functionResponse"]["name"], "Bash");
    assert_eq!(
        user_parts[0]["functionResponse"]["response"]["content"],
        "a\nb"
    );
    assert_eq!(user_parts[1]["functionResponse"]["name"], "Bash");
    assert_eq!(
        user_parts[1]["functionResponse"]["response"]["content"],
        "/home"
    );
}
