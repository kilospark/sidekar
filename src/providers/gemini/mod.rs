//! Google Gemini native provider.
//!
//! Talks directly to `generativelanguage.googleapis.com/v1beta/models/
//! {model}:streamGenerateContent?alt=sse`. OAuth not supported — Google
//! uses static API keys here; enterprise OAuth via Google Cloud is a
//! separate surface area and deferred.
//!
//! Wire shape reference:
//!   https://ai.google.dev/api/generate-content
//!   https://ai.google.dev/api/caching (commit 2)
//!
//! Key differences from the OpenAI-compat shim (Provider::OpenAiCompat
//! pointed at Google's /v1beta/openai/chat/completions) that motivate
//! having a native adapter:
//!
//!   * Access to `thinkingConfig.thinkingBudget` for cost control on
//!     2.5 models (future StreamConfig extension).
//!   * Access to `cachedContents` lifecycle — order-of-magnitude token
//!     savings on stable prefixes (added in commit 2 of this series).
//!   * Access to Gemini-native `usageMetadata` fields including
//!     `cachedContentTokenCount` so sidekar's Usage struct reports
//!     cache hits accurately.
//!   * Stable tool-call behavior without the compat shim's translation
//!     layer that occasionally drops or reshapes arguments.
//!
//! Tool-call ID note: Gemini's wire format has no tool-call IDs. Each
//! assistant `functionCall` carries only a `name`. Sidekar's
//! ContentBlock::ToolCall expects an `id` so the next turn's
//! ToolResult can reference the specific call. We synthesize stable
//! IDs deterministically: `call_{name}_{turn_local_index}`. Same-turn
//! disambiguation (two Bash calls → `call_Bash_0`, `call_Bash_1`).
//! When converting the next turn's ToolResult back to a `functionResponse`
//! part, the adapter resolves the tool_use_id back to its function name
//! via an id-map built during request construction.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{
    AssistantResponse, ChatMessage, ContentBlock, Role, StopReason, StreamConfig, StreamEvent,
    ToolDef, Usage,
};

mod cache;
mod cache_registry;

/// Streaming call to Gemini's native streamGenerateContent endpoint.
///
/// When `config.gemini_caching` is true (default for Provider::Gemini),
/// attempts to use `cachedContents` for token-cost savings on stable
/// prefixes. Algorithm:
///
///   1. Split `messages` into a stable prefix (everything except the
///      final user turn) and the new turn.
///   2. Fingerprint (model, system, tools, prefix).
///   3. Look up the fingerprint in the local registry. If hit and
///      unexpired, build the request with `cachedContent = name` and
///      only the new turn in `contents`. System prompt and tools are
///      omitted (they live in the cached object).
///   4. On miss, estimate the prefix token count. If it exceeds
///      `gemini_cache_min_tokens`, create a cache via
///      `cachedContents.create`, store the result in the registry,
///      then send the request using that cache. Creation failures
///      (4xx from too-few-tokens, network blips) fall back to the
///      uncached path silently — caching is an optimization, never a
///      failure mode.
///   5. On the next turn, a "cache not found" response (the server
///      evicted before our TTL believed it was gone) triggers a
///      registry delete and one retry without `cachedContent`.
///
/// `prompt_cache_key` is ignored for Gemini (other providers use it
/// as an Anthropic/OpenAI-shaped hint). We maintain our own registry.
#[allow(clippy::too_many_arguments)]
pub async fn stream(
    api_key: &str,
    base_url: &str,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    _prompt_cache_key: Option<&str>,
    config: &StreamConfig,
) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
    // Try cached path first; fall through to uncached on any problem.
    // The retry loop handles the "cache evicted server-side" case once.
    let mut cache_ref: Option<String> = None;
    let mut cache_fingerprint: Option<String> = None;
    if config.gemini_caching
        && let Some(prep) = prepare_cache(
            api_key,
            base_url,
            model,
            system_prompt,
            messages,
            tools,
            config,
        )
        .await
    {
        cache_ref = Some(prep.name);
        cache_fingerprint = Some(prep.fingerprint);
    }

    // Decide which messages go in `contents`. When cached, only the
    // incremental turn(s) past the cached prefix ship.
    let (effective_system, effective_tools, effective_messages) = if cache_ref.is_some() {
        let prefix_len = cacheable_prefix_len(messages);
        (
            "",      // system lives in cache
            &[][..], // tools live in cache
            &messages[prefix_len..],
        )
    } else {
        (system_prompt, tools, messages)
    };

    let response = send_generate_request(
        api_key,
        base_url,
        model,
        effective_system,
        effective_messages,
        effective_tools,
        cache_ref.as_deref(),
    )
    .await;

    // Fallback on "cache not found": server evicted before our TTL
    // expected. Evict the registry entry and retry uncached.
    let response = match response {
        Ok(r) => r,
        Err(err) if cache_ref.is_some() && is_cache_not_found_error(&err) => {
            if let Some(fp) = &cache_fingerprint {
                let _ = cache_registry::delete(fp);
            }
            eprintln!(
                "gemini cache: server-side eviction detected, retrying without cachedContent"
            );
            send_generate_request(
                api_key,
                base_url,
                model,
                system_prompt,
                messages,
                tools,
                None,
            )
            .await?
        }
        Err(e) => return Err(e),
    };

    let (tx, rx) = mpsc::unbounded_channel();
    let model_owned = model.to_string();
    tokio::spawn(async move {
        if let Err(e) = parse_sse_stream(response, &tx, &model_owned).await {
            let _ = tx.send(StreamEvent::Error {
                message: format!("{e:#}"),
            });
        }
    });
    Ok(rx)
}

/// Execute one POST to streamGenerateContent. Separated so the cache
/// fallback path can reuse it.
#[allow(clippy::too_many_arguments)]
async fn send_generate_request(
    api_key: &str,
    base_url: &str,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    cached_content_name: Option<&str>,
) -> Result<reqwest::Response> {
    let (body, _id_map) =
        build_request_body(model, system_prompt, messages, tools, cached_content_name);
    let url = format!(
        "{}/models/{}:streamGenerateContent?alt=sse",
        base_url.trim_end_matches('/'),
        model
    );
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("content-type", "application/json".parse()?);
    headers.insert("x-goog-api-key", api_key.parse()?);

    super::log_api_request(&url, &headers, &body);

    let client = super::build_streaming_client(std::time::Duration::from_secs(300))?;
    let response = client
        .post(&url)
        .headers(headers)
        .json(&body)
        .send()
        .await
        .context("failed to connect to Gemini API")?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        super::log_api_error(status, &text);
        bail!("Gemini API error ({}): {}", status, text);
    }
    Ok(response)
}

/// Detect the specific error shape Gemini returns when a referenced
/// `cachedContent` no longer exists (TTL expired on the server, or
/// the cache was manually deleted between requests). Triggers a
/// registry eviction + uncached retry.
fn is_cache_not_found_error(err: &anyhow::Error) -> bool {
    let s = format!("{err:#}");
    // Status code 404 is the common case. Message text mentioning
    // "not found" + "cachedContent" catches the less-specific
    // rendering when the server returns 400 with NOT_FOUND in body.
    s.contains("(404")
        || (s.to_lowercase().contains("cached") && s.to_lowercase().contains("not found"))
}

/// Result of the cache preparation step: a ready-to-use cache name
/// and the fingerprint that keyed it (for eviction on retry).
struct CachePrep {
    name: String,
    fingerprint: String,
}

/// Look up or create a cached content for the current prefix. Returns
/// `None` if caching is not worthwhile (prefix too small, creation
/// failed, etc.) — caller falls back to uncached mode. Never errors.
async fn prepare_cache(
    api_key: &str,
    base_url: &str,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    config: &StreamConfig,
) -> Option<CachePrep> {
    let prefix_len = cacheable_prefix_len(messages);
    if prefix_len == 0 {
        // No history to cache yet (first turn). Nothing worth caching.
        return None;
    }
    let prefix = &messages[..prefix_len];

    // Serialize the prefix components for fingerprinting. We use the
    // same message shapes the adapter already knows how to build —
    // serde_json guarantees stable field ordering on struct types
    // with #[derive(Serialize)].
    let tools_json = serde_json::to_string(tools).ok()?;
    let prefix_json = serde_json::to_string(prefix).ok()?;
    let fingerprint = cache_registry::fingerprint(model, system_prompt, &tools_json, &prefix_json);

    // Registry hit → reuse.
    if let Some(entry) = cache_registry::lookup(&fingerprint) {
        return Some(CachePrep {
            name: entry.name,
            fingerprint,
        });
    }

    // Miss: decide whether creation is worth a round trip. Rough
    // token estimate — len(json)/4 tracks the same estimator the
    // compaction module uses. If the estimate is under the minimum,
    // skip; we'd pay a creation round-trip that the server will
    // reject and nothing would be gained.
    let approx_tokens = (system_prompt.len() + tools_json.len() + prefix_json.len()) / 4;
    if (approx_tokens as u32) < config.gemini_cache_min_tokens {
        return None;
    }

    // Build the prefix contents exactly as streamGenerateContent
    // would see them — same conversion so cache lookups work when
    // the same prefix is later sent as a delta.
    let (prefix_body, _) = build_request_body(model, system_prompt, prefix, tools, None);
    let contents = prefix_body
        .get("contents")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    let tools_decls = prefix_body
        .get("tools")
        .and_then(|t| t.as_array())
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("functionDeclarations"))
        .and_then(|fd| fd.as_array())
        .cloned()
        .unwrap_or_default();
    let system_instruction = prefix_body.get("systemInstruction").cloned();

    let display_name = format!("sidekar-{}", &fingerprint[..8]);
    let created = cache::create_cache(cache::CreateCacheRequest {
        api_key,
        base_url,
        model,
        contents: &contents,
        tools: &tools_decls,
        system_instruction: system_instruction.as_ref(),
        ttl_secs: config.gemini_cache_ttl_secs,
        display_name: &display_name,
    })
    .await;

    let created = match created {
        Ok(Some(c)) => c,
        Ok(None) => return None, // 4xx — not retryable; fall back
        Err(e) => {
            eprintln!("gemini cache: create failed, continuing uncached: {e:#}");
            return None;
        }
    };

    let entry = cache_registry::CacheEntry {
        name: created.name.clone(),
        model: model.to_string(),
        fingerprint: fingerprint.clone(),
        token_count: created.token_count,
        expires_at_unix: created.expires_at_unix,
    };
    if let Err(e) = cache_registry::store(&entry) {
        eprintln!("gemini cache: registry store failed: {e:#}");
        // Server has the cache; we just can't track it locally. Use
        // it for this turn — we'll pay a creation cost again next
        // turn, but correctness is preserved.
    }

    Some(CachePrep {
        name: created.name,
        fingerprint,
    })
}

/// Return the number of leading messages that form the "stable"
/// prefix — everything up to and including the last assistant turn.
/// The current user turn (and any subsequent partial state) is the
/// incremental delta sent alongside a cache reference.
fn cacheable_prefix_len(messages: &[ChatMessage]) -> usize {
    // Walk backward from the end; the prefix is everything up to the
    // last Assistant message (inclusive). If there's no assistant
    // message yet (fresh conversation, only a user turn), the
    // prefix is empty and we don't cache.
    for (i, msg) in messages.iter().enumerate().rev() {
        if matches!(msg.role, Role::Assistant) {
            return i + 1;
        }
    }
    0
}

// ---------------------------------------------------------------------------
// Request body construction
// ---------------------------------------------------------------------------

/// Map from sidekar's `ContentBlock::ToolCall.id` to the Gemini function
/// name. Built while converting an assistant turn with `functionCall`
/// parts. Used when the following user turn carries a `ToolResult` that
/// references one of those IDs — we need the function name to emit a
/// matching `functionResponse`, because Gemini's wire format does not
/// carry call IDs.
pub type ToolIdMap = std::collections::HashMap<String, String>;

/// Build a Gemini generateContent request body from sidekar's message
/// representation. Returns the JSON body plus the id→name map that the
/// caller should stash if they need to round-trip tool results on
/// subsequent turns (the streaming adapter doesn't — new IDs are
/// synthesized from responses on each turn).
///
/// `cached_content_name`, when `Some`, sets the `cachedContent` field
/// and signals to the caller that they MUST NOT include messages,
/// tools, or systemInstruction that are already part of the cached
/// payload in the `contents` passed here. Commit 1 of this series
/// never sets it; commit 2 wires the cache lifecycle.
pub(crate) fn build_request_body(
    _model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    cached_content_name: Option<&str>,
) -> (Value, ToolIdMap) {
    let mut contents: Vec<Value> = Vec::new();
    let mut id_map: ToolIdMap = ToolIdMap::new();

    for msg in messages {
        match msg.role {
            Role::User => {
                let mut parts: Vec<Value> = Vec::new();
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } if !text.is_empty() => {
                            parts.push(json!({ "text": text }));
                        }
                        ContentBlock::Image {
                            media_type,
                            data_base64,
                            ..
                        } => {
                            parts.push(json!({
                                "inlineData": {
                                    "mimeType": media_type,
                                    "data": data_base64,
                                }
                            }));
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            // Resolve id → function name via map built
                            // from the preceding assistant turn. If
                            // unknown (shouldn't happen in a well-
                            // formed history), fall back to the id
                            // itself so the server at least sees
                            // something; it will likely reject but
                            // that's a bug we want to surface.
                            let name = id_map
                                .get(tool_use_id)
                                .cloned()
                                .unwrap_or_else(|| tool_use_id.clone());
                            let response = if *is_error {
                                json!({ "error": content })
                            } else {
                                // functionResponse.response is a free-form
                                // object; Gemini accepts arbitrary keys.
                                // `content` is the canonical one used by
                                // Google's own examples.
                                json!({ "content": content })
                            };
                            parts.push(json!({
                                "functionResponse": {
                                    "name": name,
                                    "response": response,
                                }
                            }));
                        }
                        _ => {}
                    }
                }
                if !parts.is_empty() {
                    contents.push(json!({ "role": "user", "parts": parts }));
                }
            }
            Role::Assistant => {
                let mut parts: Vec<Value> = Vec::new();
                let mut call_index = 0u32;
                // Track whether this turn contains thinking. Gemini 2.5+
                // requires `thoughtSignature` on thought text parts and on
                // functionCall parts that follow thinking. When replaying
                // history from a different provider (e.g. Claude→Gemini)
                // we don't have a valid Gemini signature, so we use the
                // documented skip sentinel.
                let has_thinking = msg.content.iter().any(|b| {
                    matches!(b, ContentBlock::Thinking { thinking, .. } if !thinking.is_empty())
                        || matches!(b, ContentBlock::Reasoning { text, .. } if !text.is_empty())
                });
                // Sentinel value documented by Google for cross-model
                // history transfer:
                // https://ai.google.dev/gemini-api/docs/thought-signatures
                const SKIP_SIG: &str = "skip_thought_signature_validator";
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } if !text.is_empty() => {
                            parts.push(json!({ "text": text }));
                        }
                        ContentBlock::Thinking {
                            thinking,
                            signature,
                        } if !thinking.is_empty() => {
                            // Replay as a thought part. Use the real
                            // signature if we captured one from Gemini;
                            // fall back to skip sentinel for cross-
                            // provider history.
                            let sig = if signature.is_empty() {
                                SKIP_SIG
                            } else {
                                signature.as_str()
                            };
                            parts.push(json!({
                                "text": thinking,
                                "thought": true,
                                "thoughtSignature": sig,
                            }));
                        }
                        ContentBlock::Reasoning { text } if !text.is_empty() => {
                            parts.push(json!({ "text": text }));
                        }
                        ContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                            thought_signature,
                        } => {
                            // Record id→name so the next user turn's
                            // ToolResult can resolve back to a
                            // functionResponse.name.
                            id_map.insert(id.clone(), name.clone());
                            let mut part = json!({
                                "functionCall": {
                                    "name": name,
                                    "args": arguments,
                                }
                            });
                            // Gemini requires thoughtSignature on
                            // functionCall parts when thinking was
                            // present in the same turn.  Use the real
                            // captured signature when available, fall
                            // back to skip sentinel for cross-provider.
                            if let Some(sig) = thought_signature {
                                part["thoughtSignature"] = json!(sig);
                            } else if has_thinking {
                                part["thoughtSignature"] = json!(SKIP_SIG);
                            }
                            parts.push(part);
                            call_index += 1;
                        }
                        _ => {}
                    }
                }
                let _ = call_index; // suppress unused — reserved for
                // future disambiguation logic if Gemini ever returns
                // IDs. For now indices are only synthesized on the
                // response path (see parse_sse_stream).
                if !parts.is_empty() {
                    contents.push(json!({ "role": "model", "parts": parts }));
                }
            }
        }
    }

    let mut body = json!({ "contents": contents });

    if !system_prompt.is_empty() && cached_content_name.is_none() {
        // When cached, system prompt is part of the cache payload and
        // must NOT be included in the generateContent call.
        body["systemInstruction"] = json!({
            "parts": [{ "text": system_prompt }],
        });
    }

    if !tools.is_empty() && cached_content_name.is_none() {
        let declarations: Vec<Value> = tools
            .iter()
            .map(|t| {
                // Gemini rejects extraneous schema properties it doesn't
                // recognize (e.g. `$schema`, `additionalProperties`).
                // input_schema is trusted sidekar-side; tools.rs keeps it
                // plain JSON Schema, which Gemini accepts verbatim.
                json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                })
            })
            .collect();
        body["tools"] = json!([{ "functionDeclarations": declarations }]);
    }

    // Safety settings: sidekar is a developer tool; downstream callers
    // want raw model output, not Google's content moderation layered
    // on top. BLOCK_NONE on all four categories disables intervention.
    // Users who need filtering should add it at a higher layer.
    body["safetySettings"] = json!([
        { "category": "HARM_CATEGORY_HARASSMENT",        "threshold": "BLOCK_NONE" },
        { "category": "HARM_CATEGORY_HATE_SPEECH",       "threshold": "BLOCK_NONE" },
        { "category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "threshold": "BLOCK_NONE" },
        { "category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "BLOCK_NONE" },
    ]);

    if let Some(name) = cached_content_name {
        // Full cache path reference: "cachedContents/abc123".
        body["cachedContent"] = json!(name);
    }

    (body, id_map)
}

// ---------------------------------------------------------------------------
// SSE stream parsing
// ---------------------------------------------------------------------------

async fn parse_sse_stream(
    response: reqwest::Response,
    tx: &mpsc::UnboundedSender<StreamEvent>,
    model: &str,
) -> Result<()> {
    let mut stream = response.bytes_stream();
    let mut decoder = super::SseDecoder::new();

    // Per-turn accumulators. Gemini's streaming delivers text deltas
    // and tool calls inside `candidates[0].content.parts`. Text is
    // chunked; functionCall parts are atomic (delivered whole).
    let mut text_accum = String::new();
    let mut thinking_accum = String::new();
    let mut thinking_sig = String::new();
    let mut content_blocks: Vec<ContentBlock> = Vec::new();
    let mut usage = Usage::default();
    let mut stop_reason = StopReason::Stop;
    // `next_tool_index` tracks how many functionCall parts we've seen
    // this turn so StreamEvent::ToolCallStart gets a monotonically
    // increasing index. Also feeds into synthesized tool IDs.
    let mut next_tool_index: usize = 0;
    // Per-name counter so two same-name calls in one turn get distinct
    // synthesized IDs (call_Bash_0, call_Bash_1). Preserves order.
    let mut name_counters: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();

    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error reading Gemini SSE chunk")?;
        decoder.push_chunk(&chunk);

        while let Some(event) = decoder.next_event() {
            let data: Value = match super::parse_sse_json(&event) {
                Some(v) => v,
                None => continue,
            };

            // Model-declared ID (if the server echoes it; Gemini does
            // not, but the shim might). Harmless either way.

            // Candidates: Gemini emits an array, but n=1 is the norm
            // for streamGenerateContent unless caller requests more.
            if let Some(candidate) = data
                .get("candidates")
                .and_then(|c| c.as_array())
                .and_then(|arr| arr.first())
            {
                if let Some(parts) = candidate
                    .get("content")
                    .and_then(|c| c.get("parts"))
                    .and_then(|p| p.as_array())
                {
                    for part in parts {
                        // Thinking parts carry `thought: true` alongside
                        // `text`. Route to ThinkingDelta so the renderer
                        // can show them separately (or hide them).
                        let is_thought = part
                            .get("thought")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        // Capture thoughtSignature from thought-text parts
                        // only (not functionCall parts — those get their
                        // own sig stored on ContentBlock::ToolCall).
                        let is_fc_part = part.get("functionCall").is_some();
                        if !is_fc_part
                            && let Some(sig) = part.get("thoughtSignature").and_then(|v| v.as_str())
                            && !sig.is_empty()
                        {
                            thinking_sig = sig.to_string();
                        }
                        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                            if text.is_empty() {
                                continue;
                            }
                            if is_thought {
                                thinking_accum.push_str(text);
                                let _ = tx.send(StreamEvent::ThinkingDelta {
                                    delta: text.to_string(),
                                });
                            } else {
                                text_accum.push_str(text);
                                let _ = tx.send(StreamEvent::TextDelta {
                                    delta: text.to_string(),
                                });
                            }
                            continue;
                        }
                        if let Some(fc) = part.get("functionCall") {
                            let name = fc
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                            // Capture the functionCall part's own
                            // thoughtSignature for replay.
                            let fc_sig = part
                                .get("thoughtSignature")
                                .and_then(|v| v.as_str())
                                .filter(|s| !s.is_empty())
                                .map(|s| s.to_string());
                            // Flush any accumulated text as its own
                            // block so ordering matches the stream
                            // (text → tool_use, or interleaved).
                            if !text_accum.is_empty() {
                                content_blocks.push(ContentBlock::Text {
                                    text: std::mem::take(&mut text_accum),
                                });
                            }
                            if !thinking_accum.is_empty() {
                                content_blocks.push(ContentBlock::Thinking {
                                    thinking: std::mem::take(&mut thinking_accum),
                                    signature: std::mem::take(&mut thinking_sig),
                                });
                            }
                            // Synthesize a stable ID. Per-name index
                            // disambiguates two same-name calls in one
                            // turn. `call_` prefix keeps us compatible
                            // with places that expect OpenAI-shaped IDs.
                            let counter = name_counters.entry(name.clone()).or_insert(0);
                            let id = format!("call_{name}_{}", *counter);
                            *counter += 1;
                            let index = next_tool_index;
                            next_tool_index += 1;

                            let _ = tx.send(StreamEvent::ToolCallStart {
                                index,
                                id: id.clone(),
                                name: name.clone(),
                            });
                            let _ = tx.send(StreamEvent::ToolCallDelta {
                                index,
                                delta: args.to_string(),
                            });
                            let _ = tx.send(StreamEvent::ToolCallEnd { index });

                            content_blocks.push(ContentBlock::ToolCall {
                                id,
                                name,
                                arguments: args,
                                thought_signature: fc_sig,
                            });
                            continue;
                        }
                        // inlineData (image output) — not emitted by
                        // current Gemini models in streamGenerateContent
                        // responses. If/when supported we'd push a
                        // ContentBlock::Image. No-op for now.
                    }
                }

                // finishReason arrives on the last chunk. Map to
                // sidekar's StopReason. Safety stops emit with no
                // content, which the agent loop may interpret as a
                // stall — surfaced via a log note below.
                if let Some(reason) = candidate.get("finishReason").and_then(|v| v.as_str()) {
                    stop_reason = match reason {
                        "STOP" => StopReason::Stop,
                        "MAX_TOKENS" => StopReason::Length,
                        "SAFETY" | "RECITATION" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII" => {
                            // Log the reason so it shows up in proxy
                            // captures / debug output. Agent loop will
                            // see an empty assistant turn and likely
                            // bail with "no output" — acceptable.
                            eprintln!("gemini: stream ended with content-policy stop: {reason}");
                            StopReason::Stop
                        }
                        "OTHER" | "MALFORMED_FUNCTION_CALL" => {
                            eprintln!("gemini: stream ended abnormally: {reason}");
                            StopReason::Error
                        }
                        _ => StopReason::Stop,
                    };
                }
            }

            // usageMetadata can appear on any chunk; Gemini typically
            // sends it on the final chunk. Always take the latest
            // observation so we end with accurate totals.
            if let Some(meta) = data.get("usageMetadata") {
                // promptTokenCount = total input (incl. cached).
                // cachedContentTokenCount = the cached portion (a subset
                // of prompt). We map cached portion to
                // Usage::cache_read_tokens and subtract from input so
                // the two don't double-count.
                let prompt = meta
                    .get("promptTokenCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let cached = meta
                    .get("cachedContentTokenCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let output = meta
                    .get("candidatesTokenCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                usage.cache_read_tokens = cached;
                usage.input_tokens = prompt.saturating_sub(cached);
                usage.output_tokens = output;
                // cache_write_tokens stays 0: cache creation happens
                // through a separate API call (see commit 2), not via
                // generateContent. promptTokenCount never reflects
                // cache-creation work on this endpoint.
            }
        }
    }

    // Drain any trailing text / thinking that weren't followed by a
    // functionCall or finishReason flush.
    if !text_accum.is_empty() {
        content_blocks.push(ContentBlock::Text { text: text_accum });
    }
    if !thinking_accum.is_empty() {
        content_blocks.push(ContentBlock::Thinking {
            thinking: thinking_accum,
            signature: thinking_sig,
        });
    }

    // If we emitted tool calls but the server didn't report a specific
    // stop reason, mark it as ToolUse so the agent loop runs them.
    if content_blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolCall { .. }))
        && matches!(stop_reason, StopReason::Stop)
    {
        stop_reason = StopReason::ToolUse;
    }

    let _ = tx.send(StreamEvent::Done {
        message: AssistantResponse {
            content: content_blocks,
            usage,
            stop_reason,
            model: model.to_string(),
            response_id: String::new(),
            rate_limit: None,
        },
    });

    Ok(())
}

// ---------------------------------------------------------------------------
// Model list
// ---------------------------------------------------------------------------

/// Fetch Gemini's model list. GET /v1beta/models?pageSize=100.
pub async fn fetch_gemini_model_list(api_key: &str) -> Result<Vec<super::RemoteModel>, String> {
    let url = "https://generativelanguage.googleapis.com/v1beta/models?pageSize=100";
    let client = super::catalog_http_client(super::MODEL_CATALOG_TIMEOUT_SECS)?;
    let body: Value =
        super::catalog_send_json_plain(client.get(url).header("x-goog-api-key", api_key), "Gemini")
            .await?;
    let Some(models) = body.get("models").and_then(|m| m.as_array()) else {
        return Ok(Vec::new());
    };
    Ok(models
        .iter()
        .filter_map(|m| {
            // Gemini returns names as "models/gemini-2.5-pro"; strip
            // the prefix. Skip non-gemini entries (text-bison, embed).
            let name = m.get("name").and_then(|v| v.as_str())?;
            let id = name.strip_prefix("models/")?.to_string();
            if !id.starts_with("gemini-") {
                return None;
            }
            // Only keep models that support generateContent (some are
            // embedding-only or list-only).
            let supports_generate = m
                .get("supportedGenerationMethods")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().any(|s| s.as_str() == Some("generateContent")))
                .unwrap_or(true);
            if !supports_generate {
                return None;
            }
            let context_window = m
                .get("inputTokenLimit")
                .and_then(|v| v.as_u64())
                .unwrap_or(128_000) as u32;
            let display_name = m
                .get("displayName")
                .and_then(|v| v.as_str())
                .unwrap_or(&id)
                .to_string();
            Some(super::RemoteModel::catalog(id, display_name, context_window))
        })
        .collect())
}

/// Fetch context window + max output tokens for a specific Gemini model.
pub async fn fetch_gemini_model_limits(
    api_key: &str,
    base_url: &str,
    model: &str,
) -> Option<(u32, u32)> {
    let url = format!("{}/models/{}", base_url.trim_end_matches('/'), model);
    let client = super::catalog_http_client(super::MODEL_CATALOG_TIMEOUT_SECS).ok()?;
    let resp = client
        .get(&url)
        .header("x-goog-api-key", api_key)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    let ctx = body.get("inputTokenLimit").and_then(|v| v.as_u64())? as u32;
    let out = body
        .get("outputTokenLimit")
        .and_then(|v| v.as_u64())
        .unwrap_or(8192) as u32;
    Some((ctx, out))
}

#[cfg(test)]
mod tests;
