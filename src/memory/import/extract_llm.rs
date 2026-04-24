//! LLM-based extraction for freeform text sources. Given a blob
//! of text (a `CLAUDE.md`, `.cursorrules`, a concatenated
//! session transcript...) and a provider handle, produce zero
//! or more typed memory candidates.
//!
//! Prompt is adapted from nairo's `PREFERENCE_EXTRACTION_PROMPT`
//! with stricter output-shape requirements so a Rust serde_json
//! parse cannot silently accept weird outputs.
//!
//! Callers must provide the `Provider` and model. The CLI path in
//! `commands.rs` resolves both from `--credential` / `--model` (or
//! falls back to sensible defaults) before invoking extraction.

use super::Candidate;
use crate::providers::{ChatMessage, ContentBlock, Provider, Role, StreamEvent};
use anyhow::{Result, anyhow};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Char cap per single LLM call. ~24k chars ≈ ~6k tokens, matches
/// nairo's cap. Content longer than this is tail-truncated
/// (recent activity is more valuable than the beginning of a
/// multi-hour session).
const MAX_INPUT_CHARS: usize = 24_000;

const SYSTEM_PROMPT: &str = "\
You extract durable user preferences, conventions, and workflow patterns \
from AI coding tool configuration and conversation history.

Rules:
- Only extract statements that represent LASTING preferences the user \
  wants remembered across future sessions.
- Ignore one-time debugging instructions, questions, status updates, and \
  operational chatter.
- Restate each memory as a clean, specific imperative sentence.
- Assign a confidence score (0.0-1.0) reflecting how clearly this is a \
  durable preference vs a one-off instruction. Floor 0.4.
- Only use types: \"preference\" (personal taste), \"convention\" \
  (project/code standards), \"constraint\" (things to avoid/never do), \
  \"decision\" (architectural choices).
- If no durable preferences exist, return { \"memories\": [] }.

Return valid JSON exactly matching this schema:
{ \"memories\": [ { \"summary\": string, \"type\": string, \
\"confidence\": number, \"evidence\": string } ] }

Do not wrap the JSON in markdown fences or add commentary.";

#[derive(Debug, Clone, Deserialize)]
struct LlmResponse {
    memories: Vec<LlmMemory>,
}

#[derive(Debug, Clone, Deserialize)]
struct LlmMemory {
    summary: String,
    #[serde(rename = "type")]
    mem_type: String,
    confidence: f64,
    #[serde(default)]
    evidence: String,
}

const VALID_TYPES: &[&str] = &["preference", "convention", "constraint", "decision"];

/// Extract typed memories from a single text blob using the
/// provider. Caller is responsible for choosing scope / project
/// and attaching the final `Candidate` tags.
pub(super) async fn extract_from_text(
    provider: &Provider,
    model: &str,
    source_kind: &str,
    source_file: &Path,
    project: &str,
    scope: &str,
    content: &str,
) -> Result<Vec<Candidate>> {
    let trimmed = if content.chars().count() > MAX_INPUT_CHARS {
        // Keep the tail — for session transcripts the most recent
        // turns carry the durable signals; for static config this
        // just caps cost.
        let skip = content.chars().count() - MAX_INPUT_CHARS;
        content.chars().skip(skip).collect::<String>()
    } else {
        content.to_string()
    };

    let user_prompt = format!(
        "Source file: {}\n\n{}",
        source_file.display(),
        trimmed.trim()
    );

    let raw = call_provider(provider, model, &user_prompt).await?;
    let parsed = parse_response(&raw)?;

    Ok(parsed
        .memories
        .into_iter()
        .filter_map(|m| to_candidate(m, project, scope, source_kind, source_file))
        .collect())
}

fn to_candidate(
    m: LlmMemory,
    project: &str,
    scope: &str,
    source_kind: &str,
    source_file: &Path,
) -> Option<Candidate> {
    if m.summary.trim().chars().count() < 10 {
        return None;
    }
    if !VALID_TYPES.contains(&m.mem_type.as_str()) {
        return None;
    }
    let confidence = m.confidence.clamp(0.4, 1.0);
    let mut tags = vec![
        "imported".to_string(),
        source_kind.replace(':', "-"),
    ];
    if !m.evidence.trim().is_empty() {
        tags.push("has-evidence".to_string());
    }
    Some(Candidate {
        event_type: m.mem_type,
        summary: m.summary.trim().to_string(),
        scope: scope.to_string(),
        project: project.to_string(),
        confidence,
        tags,
        source_kind: source_kind.to_string(),
        source_file: PathBuf::from(source_file),
    })
}

/// Non-streaming LLM call via the existing streaming Provider API.
/// Follows the exact pattern in `repl::journal::task::call_summarizer`
/// — drain TextDelta events into a string until Done.
async fn call_provider(provider: &Provider, model: &str, user_prompt: &str) -> Result<String> {
    let messages = vec![ChatMessage {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: user_prompt.to_string(),
        }],
    }];

    let (mut rx, _reclaim) = provider
        .stream(
            model,
            SYSTEM_PROMPT,
            &messages,
            &[],
            Some("sidekar-memory-import"),
            None,
            None,
        )
        .await?;

    let mut text = String::new();
    let mut last_error: Option<String> = None;
    while let Some(event) = rx.recv().await {
        match event {
            StreamEvent::TextDelta { delta } => text.push_str(&delta),
            StreamEvent::Error { message } => last_error = Some(message),
            StreamEvent::Done { .. } => break,
            _ => {}
        }
    }

    if let Some(err) = last_error {
        return Err(anyhow!("LLM extraction stream error: {err}"));
    }
    if text.is_empty() {
        return Err(anyhow!("LLM extraction returned empty response"));
    }
    Ok(text)
}

/// Parse the raw model output. Tolerates ```json fences and
/// leading / trailing whitespace because providers vary in
/// strictness of JSON-mode honoring.
fn parse_response(raw: &str) -> Result<LlmResponse> {
    let cleaned = strip_code_fence(raw.trim());
    serde_json::from_str::<LlmResponse>(cleaned).map_err(|e| {
        anyhow!(
            "failed to parse LLM JSON response: {e}; raw (first 400 chars): {}",
            cleaned.chars().take(400).collect::<String>()
        )
    })
}

fn strip_code_fence(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix("```json") {
        let rest = rest.trim_start_matches('\n');
        if let Some(end) = rest.rfind("```") {
            return &rest[..end];
        }
    }
    if let Some(rest) = s.strip_prefix("```") {
        let rest = rest.trim_start_matches('\n');
        if let Some(end) = rest.rfind("```") {
            return &rest[..end];
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_response_accepts_clean_json() {
        let raw = r#"{"memories":[{"summary":"Always prefer TypeScript interfaces over type aliases.","type":"convention","confidence":0.85,"evidence":"from CLAUDE.md line 4"}]}"#;
        let got = parse_response(raw).unwrap();
        assert_eq!(got.memories.len(), 1);
        assert_eq!(got.memories[0].mem_type, "convention");
    }

    #[test]
    fn parse_response_strips_markdown_fence() {
        let raw = "```json\n{\"memories\":[]}\n```";
        let got = parse_response(raw).unwrap();
        assert!(got.memories.is_empty());
    }

    #[test]
    fn parse_response_strips_bare_fence() {
        let raw = "```\n{\"memories\":[]}\n```";
        let got = parse_response(raw).unwrap();
        assert!(got.memories.is_empty());
    }

    #[test]
    fn parse_response_rejects_non_json() {
        assert!(parse_response("I am not json").is_err());
    }

    #[test]
    fn to_candidate_rejects_short_summaries() {
        let m = LlmMemory {
            summary: "too short".to_string(),
            mem_type: "preference".to_string(),
            confidence: 0.9,
            evidence: String::new(),
        };
        assert!(
            to_candidate(
                m,
                "demo",
                crate::scope::PROJECT_SCOPE,
                "import:claude:md",
                std::path::Path::new("/tmp/CLAUDE.md"),
            )
            .is_none()
        );
    }

    #[test]
    fn to_candidate_rejects_invalid_type() {
        let m = LlmMemory {
            summary: "Always use four-space indentation in Python files.".to_string(),
            mem_type: "quirk".to_string(),
            confidence: 0.9,
            evidence: String::new(),
        };
        assert!(
            to_candidate(
                m,
                "demo",
                crate::scope::PROJECT_SCOPE,
                "import:claude:md",
                std::path::Path::new("/tmp/CLAUDE.md"),
            )
            .is_none()
        );
    }

    #[test]
    fn to_candidate_clamps_low_confidence() {
        let m = LlmMemory {
            summary: "Always prefer TypeScript interfaces over type aliases.".to_string(),
            mem_type: "convention".to_string(),
            confidence: 0.1,
            evidence: "from readme".to_string(),
        };
        let got = to_candidate(
            m,
            "demo",
            crate::scope::PROJECT_SCOPE,
            "import:claude:md",
            std::path::Path::new("/tmp/CLAUDE.md"),
        )
        .expect("should produce candidate");
        assert!((got.confidence - 0.4).abs() < f64::EPSILON);
        assert!(got.tags.contains(&"has-evidence".to_string()));
    }

    #[test]
    fn to_candidate_preserves_good_confidence() {
        let m = LlmMemory {
            summary: "Never commit generated files to git.".to_string(),
            mem_type: "constraint".to_string(),
            confidence: 0.92,
            evidence: String::new(),
        };
        let got = to_candidate(
            m,
            "demo",
            crate::scope::PROJECT_SCOPE,
            "import:claude:md",
            std::path::Path::new("/tmp/CLAUDE.md"),
        )
        .unwrap();
        assert!((got.confidence - 0.92).abs() < 1e-9);
        assert_eq!(got.event_type, "constraint");
        assert!(!got.tags.contains(&"has-evidence".to_string()));
    }
}
