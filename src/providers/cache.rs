use serde_json::Value;

/// Creates a cache control marker with optional TTL and scope
pub fn create_stable_marker(cache_ttl: Option<&String>, cache_scope: Option<&String>) -> Value {
    let mut marker = serde_json::json!({ "type": "ephemeral" });
    if let Some(ttl) = cache_ttl {
        marker["ttl"] = serde_json::json!(ttl);
    }
    if let Some(scope) = cache_scope {
        marker["scope"] = serde_json::json!(scope);
    }
    marker
}

/// Creates a rolling cache control marker with optional TTL (no scope)
pub fn create_rolling_marker(cache_ttl: Option<&String>) -> Value {
    let mut marker = serde_json::json!({ "type": "ephemeral" });
    if let Some(ttl) = cache_ttl {
        marker["ttl"] = serde_json::json!(ttl);
    }
    marker
}

/// Applies cache control to tools (last tool definition)
pub fn apply_tools_cache_control(tools: &mut Option<Vec<Value>>, marker: &Value) -> bool {
    let Some(tools) = tools.as_mut() else {
        return false;
    };
    let Some(last) = tools.last_mut() else {
        return false;
    };
    last["cache_control"] = marker.clone();
    true
}

/// Applies cache control to system blocks (last text block)
pub fn apply_system_cache_control(system: &mut [Value], marker: &Value) -> bool {
    for block in system.iter_mut().rev() {
        if block.get("type").and_then(|v| v.as_str()) == Some("text") {
            block["cache_control"] = marker.clone();
            return true;
        }
    }
    false
}

/// Applies cache control to the latest message
pub fn apply_message_cache_control(message: &mut Value, marker: &Value) -> bool {
    let Some(content) = message.get_mut("content") else {
        return false;
    };

    if let Some(text) = content.as_str() {
        let text = text.to_string();
        if text.is_empty() {
            return false;
        }
        *content = serde_json::json!([{
            "type": "text",
            "text": text,
            "cache_control": marker,
        }]);
        return true;
    }

    let Some(parts) = content.as_array_mut() else {
        return false;
    };

    for part in parts.iter_mut().rev() {
        match part.get("type").and_then(|v| v.as_str()) {
            Some("text") => {
                let text = part.get("text").and_then(|v| v.as_str()).unwrap_or("");
                if text.is_empty() {
                    continue;
                }
                part["cache_control"] = marker.clone();
                return true;
            }
            Some("tool_result") => {
                part["cache_control"] = marker.clone();
                return true;
            }
            _ => {}
        }
    }

    false
}

/// Determines if a model supports Anthropic-style cache control
pub fn supports_anthropic_cache_control(model: &str) -> bool {
    model.to_ascii_lowercase().contains("claude")
}

/// Adds Anthropic cache control to OpenAI-compatible request bodies
pub fn maybe_add_anthropic_cache_control(model: &str, body: &mut Value) {
    if !supports_anthropic_cache_control(model) {
        return;
    }

    let Some(messages) = body.get_mut("messages").and_then(|v| v.as_array_mut()) else {
        return;
    };

    for msg in messages.iter_mut().rev() {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role != "user" && role != "assistant" {
            continue;
        }

        let Some(content) = msg.get_mut("content") else {
            continue;
        };

        if let Some(text) = content.as_str() {
            *content = serde_json::json!([{
                "type": "text",
                "text": text,
                "cache_control": {"type": "ephemeral"},
            }]);
            return;
        }

        let Some(parts) = content.as_array_mut() else {
            continue;
        };

        for part in parts.iter_mut().rev() {
            if part.get("type").and_then(|v| v.as_str()) == Some("text") {
                part["cache_control"] = serde_json::json!({"type": "ephemeral"});
                return;
            }
        }
    }
}