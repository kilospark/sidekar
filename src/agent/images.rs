//! One-turn-only user images: materialize paths, cite paths for the API, strip blobs after a turn.

use anyhow::Context;
use base64::Engine;

use crate::providers::{ChatMessage, ContentBlock, Role};

fn ext_for_media_type(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "bin",
    }
}

/// If an image block has bytes but no path, write a temp file and set `source_path`.
pub fn materialize_ephemeral_image_sources(blocks: &mut Vec<ContentBlock>) -> anyhow::Result<()> {
    for block in blocks.iter_mut() {
        let ContentBlock::Image {
            media_type,
            data_base64,
            source_path,
        } = block
        else {
            continue;
        };
        if data_base64.is_empty() || source_path.as_ref().is_some_and(|s| !s.is_empty()) {
            continue;
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data_base64.as_str())
            .context("decode image base64 for temp file")?;
        let ext = ext_for_media_type(media_type.as_str());
        let name = format!(
            "sidekar-img-{}-{:016x}.{}",
            std::process::id(),
            rand::random::<u64>(),
            ext
        );
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, &bytes).with_context(|| format!("write {}", path.display()))?;
        *source_path = Some(path.to_string_lossy().to_string());
    }
    Ok(())
}

fn text_block_contains_path(prev: Option<&ContentBlock>, p: &str) -> bool {
    matches!(
        prev,
        Some(ContentBlock::Text { text }) if text.contains(p)
    )
}

/// Insert a path citation before each inline image when the model should know where to re-read bytes.
pub fn affix_image_path_citations(blocks: &mut Vec<ContentBlock>) {
    let mut i = 0usize;
    while i < blocks.len() {
        let (need, path) = match &blocks[i] {
            ContentBlock::Image {
                data_base64,
                source_path: Some(p),
                ..
            } if !data_base64.is_empty() && !p.is_empty() => {
                let prev = i.checked_sub(1).and_then(|j| blocks.get(j));
                (!text_block_contains_path(prev, p.as_str()), p.clone())
            }
            _ => (false, String::new()),
        };
        if need {
            blocks.insert(
                i,
                ContentBlock::Text {
                    text: format!(
                        "[Image file on disk (inline pixels sent this turn only; re-read or base64-encode if needed): {path}]"
                    ),
                },
            );
            i += 2;
        } else {
            i += 1;
        }
    }
}

/// After a successful model turn, replace user image payloads with path-only text.
pub fn strip_user_image_blobs_from_history(history: &mut Vec<ChatMessage>) {
    for msg in history.iter_mut() {
        if msg.role != Role::User {
            continue;
        }
        strip_user_message_image_blobs(&mut msg.content);
    }
}

fn strip_user_message_image_blobs(content: &mut Vec<ContentBlock>) {
    let mut out = Vec::with_capacity(content.len());
    for block in std::mem::take(content) {
        match block {
            ContentBlock::Image {
                data_base64,
                source_path,
                ..
            } if !data_base64.is_empty() => {
                let text = source_path
                    .filter(|p| !p.trim().is_empty())
                    .map(|p| {
                        format!(
                            "[Image inline data cleared after first turn; read from disk if needed: {p}]"
                        )
                    })
                    .unwrap_or_else(|| {
                        "[Image inline data cleared after first turn; path was not recorded]"
                            .to_string()
                    });
                out.push(ContentBlock::Text { text });
            }
            other => out.push(other),
        }
    }
    *content = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_replaces_image_with_path_text() {
        let mut history = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Image {
                media_type: "image/png".into(),
                data_base64: "abcd".into(),
                source_path: Some("/tmp/x.png".into()),
            }],
        }];
        strip_user_image_blobs_from_history(&mut history);
        assert_eq!(history[0].content.len(), 1);
        match &history[0].content[0] {
            ContentBlock::Text { text } => {
                assert!(text.contains("/tmp/x.png"));
            }
            _ => panic!("expected text"),
        }
    }
}
