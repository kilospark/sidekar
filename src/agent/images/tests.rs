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
