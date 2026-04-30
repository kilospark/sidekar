//! Parsers for the JSONL / JSON session transcript formats we know
//! about. Each parser returns a `SessionTranscript` that downstream
//! LLM extraction can concatenate into a single prompt.
//!
//! Keeping the turn format provider-native (role + text) lets us
//! use the same LLM extractor across all sources.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// One turn from a captured session.
#[derive(Debug, Clone)]
pub(super) struct Turn {
    pub role: String,
    pub text: String,
}

/// Enough info for a single transcript to produce a candidate.
/// `cwd` is the directory the session ran in (used for project
/// scoping). `turns` is already filtered to user + assistant roles
/// with non-empty text.
#[derive(Debug, Clone)]
pub(super) struct SessionTranscript {
    pub source_path: PathBuf,
    pub cwd: Option<PathBuf>,
    pub turns: Vec<Turn>,
}

impl SessionTranscript {
    /// Flatten turns into a single string suitable for LLM input.
    /// Per-turn header makes it easy for the model to attribute
    /// extracted preferences back to the user (ignore assistant
    /// output when deciding what to remember).
    pub fn concatenated(&self, max_chars: usize) -> String {
        let mut buf = String::new();
        for turn in &self.turns {
            let line = format!("[{}]\n{}\n\n", turn.role, turn.text.trim());
            if buf.chars().count() + line.chars().count() > max_chars {
                // Keep tail-weighting: prefer recent turns over
                // early ones when trimming. Caller passed the cap.
                let drop = buf.chars().count() + line.chars().count() - max_chars;
                if drop >= buf.chars().count() {
                    buf.clear();
                } else {
                    buf = buf.chars().skip(drop).collect();
                }
            }
            buf.push_str(&line);
        }
        buf
    }
}

// ---- Claude ---------------------------------------------------------------

/// Each line of Claude's JSONL is a `RawEntry`. The shape varies a
/// lot (queue ops, snapshots, user messages, assistant messages,
/// tool uses, thinking blocks) — we only keep the ones with real
/// text content for the user or assistant roles.
#[derive(Debug, Deserialize)]
struct ClaudeEntry {
    #[serde(rename = "type")]
    entry_type: Option<String>,
    message: Option<ClaudeMessage>,
    cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeMessage {
    role: Option<String>,
    content: Option<Value>,
}

pub(super) fn parse_claude_jsonl(path: &Path) -> Result<SessionTranscript> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut turns = Vec::new();
    let mut cwd = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: ClaudeEntry = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if cwd.is_none()
            && let Some(c) = entry.cwd.as_ref()
            && !c.is_empty()
        {
            cwd = Some(PathBuf::from(c));
        }
        if !matches!(
            entry.entry_type.as_deref(),
            Some("user") | Some("assistant")
        ) {
            continue;
        }
        let Some(msg) = entry.message else { continue };
        let Some(role) = msg.role else { continue };
        if !matches!(role.as_str(), "user" | "assistant") {
            continue;
        }
        let Some(content) = msg.content else { continue };
        if let Some(text) = extract_claude_text(&content)
            && !text.trim().is_empty()
        {
            turns.push(Turn { role, text });
        }
    }

    Ok(SessionTranscript {
        source_path: path.to_path_buf(),
        cwd,
        turns,
    })
}

fn extract_claude_text(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => {
            let mut parts = Vec::new();
            for item in items {
                if let Some(obj) = item.as_object() {
                    let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
                    if kind == "text"
                        && let Some(text) = obj.get("text").and_then(Value::as_str)
                        && !text.is_empty()
                    {
                        parts.push(text.to_string());
                    }
                    // Skip tool_use, thinking, tool_result — noisy
                    // and rarely carries the durable preference.
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        _ => None,
    }
}

// ---- Codex ----------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CodexEntry {
    #[serde(rename = "type")]
    entry_type: Option<String>,
    payload: Option<Value>,
}

pub(super) fn parse_codex_jsonl(path: &Path) -> Result<SessionTranscript> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut turns = Vec::new();
    let mut cwd = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: CodexEntry = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let entry_type = entry.entry_type.as_deref().unwrap_or("");
        let Some(payload) = entry.payload else {
            continue;
        };

        // session_meta — grab the cwd.
        if entry_type == "session_meta"
            && let Some(c) = payload.get("cwd").and_then(Value::as_str)
            && !c.is_empty()
        {
            cwd = Some(PathBuf::from(c));
            continue;
        }

        // event_msg -> user_message / agent_message are the real turns.
        if entry_type == "event_msg"
            && let Some(inner_type) = payload.get("type").and_then(Value::as_str)
        {
            match inner_type {
                "user_message" => {
                    if let Some(m) = payload.get("message").and_then(Value::as_str)
                        && !m.trim().is_empty()
                    {
                        turns.push(Turn {
                            role: "user".to_string(),
                            text: m.to_string(),
                        });
                    }
                }
                "agent_message" => {
                    if let Some(m) = payload.get("message").and_then(Value::as_str)
                        && !m.trim().is_empty()
                    {
                        turns.push(Turn {
                            role: "assistant".to_string(),
                            text: m.to_string(),
                        });
                    }
                }
                _ => {}
            }
        }

        // response_item -> message with developer/user role. Explicitly
        // skip base_instructions / sandbox prompts (developer role).
        if entry_type == "response_item"
            && payload.get("type").and_then(Value::as_str) == Some("message")
            && let Some(role) = payload.get("role").and_then(Value::as_str)
            && matches!(role, "user" | "assistant")
            && let Some(content) = payload.get("content")
            && let Some(text) = extract_codex_text(content)
            && !text.trim().is_empty()
        {
            turns.push(Turn {
                role: role.to_string(),
                text,
            });
        }
    }

    Ok(SessionTranscript {
        source_path: path.to_path_buf(),
        cwd,
        turns,
    })
}

fn extract_codex_text(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => {
            let mut parts = Vec::new();
            for item in items {
                if let Some(obj) = item.as_object() {
                    let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
                    if matches!(kind, "input_text" | "output_text")
                        && let Some(text) = obj.get("text").and_then(Value::as_str)
                        && !text.is_empty()
                    {
                        parts.push(text.to_string());
                    }
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        _ => None,
    }
}

// ---- Gemini ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GeminiFile {
    messages: Option<Vec<GeminiMessage>>,
}

#[derive(Debug, Deserialize)]
struct GeminiMessage {
    #[serde(rename = "type")]
    msg_type: Option<String>,
    content: Option<Value>,
}

pub(super) fn parse_gemini_json(path: &Path) -> Result<SessionTranscript> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let file: GeminiFile =
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let mut turns = Vec::new();
    for msg in file.messages.unwrap_or_default() {
        let role = match msg.msg_type.as_deref() {
            Some("user") => "user",
            Some("model") | Some("assistant") => "assistant",
            _ => continue,
        };
        let Some(content) = msg.content else { continue };
        let text = match &content {
            Value::String(s) => s.clone(),
            Value::Array(items) => items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str).map(String::from))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        turns.push(Turn {
            role: role.to_string(),
            text,
        });
    }

    // Gemini stores project via the parent dir name (`.gemini/tmp/<project>/chats/...`).
    let cwd = path.parent().and_then(|p| p.parent()).map(PathBuf::from);
    Ok(SessionTranscript {
        source_path: path.to_path_buf(),
        cwd,
        turns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_tmp(name: &str, body: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sidekar-transcript-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join(name);
        fs::write(&path, body).expect("write fixture");
        path
    }

    #[test]
    fn parse_claude_jsonl_extracts_user_and_text_content() {
        let body = concat!(
            r#"{"type":"file-history-snapshot","messageId":"x"}"#,
            "\n",
            r#"{"type":"user","cwd":"/p","message":{"role":"user","content":"Hello"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","text":"..."},{"type":"text","text":"Hi"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use"}]}}"#,
            "\n",
        );
        let path = write_tmp("claude.jsonl", body);
        let t = parse_claude_jsonl(&path).unwrap();
        assert_eq!(t.cwd.as_deref(), Some(Path::new("/p")));
        assert_eq!(t.turns.len(), 2);
        assert_eq!(t.turns[0].role, "user");
        assert_eq!(t.turns[0].text, "Hello");
        assert_eq!(t.turns[1].role, "assistant");
        assert_eq!(t.turns[1].text, "Hi");
    }

    #[test]
    fn parse_claude_jsonl_skips_bad_lines() {
        let body = concat!(
            "not json\n",
            r#"{"type":"user","message":{"role":"user","content":"ok"}}"#,
            "\n",
        );
        let path = write_tmp("claude-bad.jsonl", body);
        let t = parse_claude_jsonl(&path).unwrap();
        assert_eq!(t.turns.len(), 1);
    }

    #[test]
    fn parse_codex_jsonl_picks_up_cwd_and_user_agent_messages() {
        let body = concat!(
            r#"{"type":"session_meta","payload":{"cwd":"/Users/me/demo"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"hello"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"agent_message","message":"hi back"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"task_started"}}"#,
            "\n",
        );
        let path = write_tmp("codex.jsonl", body);
        let t = parse_codex_jsonl(&path).unwrap();
        assert_eq!(t.cwd.as_deref(), Some(Path::new("/Users/me/demo")));
        assert_eq!(t.turns.len(), 2);
        assert_eq!(t.turns[0].role, "user");
        assert_eq!(t.turns[1].role, "assistant");
    }

    #[test]
    fn parse_codex_jsonl_skips_developer_role_messages() {
        // Developer messages are system prompts / sandbox
        // instructions. They must never become "user" memories.
        let body = concat!(
            r#"{"type":"session_meta","payload":{"cwd":"/p"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions ...>"}]}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"real user turn"}}"#,
            "\n",
        );
        let path = write_tmp("codex-dev.jsonl", body);
        let t = parse_codex_jsonl(&path).unwrap();
        assert_eq!(t.turns.len(), 1);
        assert_eq!(t.turns[0].role, "user");
        assert_eq!(t.turns[0].text, "real user turn");
    }

    #[test]
    fn parse_gemini_json_maps_user_and_model() {
        let body = r#"{
            "sessionId":"abc",
            "messages":[
                {"type":"user","content":[{"text":"hi"}]},
                {"type":"model","content":[{"text":"yo"}]},
                {"type":"tool","content":"skip"}
            ]
        }"#;
        let path = write_tmp("gemini.json", body);
        let t = parse_gemini_json(&path).unwrap();
        assert_eq!(t.turns.len(), 2);
        assert_eq!(t.turns[0].role, "user");
        assert_eq!(t.turns[1].role, "assistant");
    }

    #[test]
    fn concatenated_tail_weights_on_overflow() {
        let t = SessionTranscript {
            source_path: PathBuf::from("/tmp/x"),
            cwd: None,
            turns: vec![
                Turn {
                    role: "user".into(),
                    text: "A".repeat(100),
                },
                Turn {
                    role: "assistant".into(),
                    text: "B".repeat(100),
                },
                Turn {
                    role: "user".into(),
                    text: "CRECENT".into(),
                },
            ],
        };
        let out = t.concatenated(80);
        // Always keeps the most recent content.
        assert!(out.contains("CRECENT"));
        // Drops the oldest user turn.
        assert!(!out.starts_with("[user]\nAA"));
    }
}
