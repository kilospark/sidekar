use super::*;

#[test]
fn human_size_scales_and_admits_when_there_is_none() {
    assert_eq!(human_size(Some("512")), "512B");
    assert_eq!(human_size(Some("2048")), "2.0KB");
    assert_eq!(human_size(Some("5242880")), "5.0MB");
    // Google-native docs report no size at all; guessing one would be a lie.
    assert_eq!(human_size(None), "-");
    assert_eq!(human_size(Some("not-a-number")), "-");
}

#[test]
fn str_at_never_panics_on_a_missing_field() {
    let v = serde_json::json!({"id": "abc"});
    assert_eq!(str_at(&v, "id"), "abc");
    assert_eq!(str_at(&v, "name"), "");
}

#[test]
fn google_native_files_are_exported_real_files_are_not() {
    // A Doc has no bytes to download; a PDF does, and exporting it would be
    // wrong.
    assert_eq!(
        export_mime_for("application/vnd.google-apps.document"),
        Some("text/plain")
    );
    assert_eq!(
        export_mime_for("application/vnd.google-apps.spreadsheet"),
        Some("text/csv")
    );
    assert_eq!(export_mime_for("application/pdf"), None);
    assert_eq!(
        export_mime_for("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        None
    );
}

#[test]
fn binary_is_not_mistaken_for_text() {
    // %PDF- then a NUL: printing this would corrupt the file and the terminal.
    assert!(!looks_like_text(b"%PDF-1.4\x00\x01\x02"));
    // PK zip header with invalid UTF-8, which is what a .docx opens with.
    assert!(!looks_like_text(&[0x50, 0x4B, 0x03, 0x04, 0xFF, 0xFE]));
}

#[test]
fn real_text_is_still_printable() {
    assert!(looks_like_text(b"name,qty\nwidget,3\n"));
    assert!(looks_like_text("a document with accents: café".as_bytes()));
    assert!(looks_like_text(b""));
}

#[test]
fn the_utf8_replacement_that_caused_this_is_caught_by_a_size_check() {
    // Decoding binary as UTF-8 turns each invalid byte into U+FFFD, three bytes
    // where one stood. That is the inflation raven measured at 39% and 83%, and
    // why the download now compares against Drive's reported size.
    let original: Vec<u8> = vec![0x50, 0x4B, 0x03, 0x04, 0xFF, 0xFE, 0x9C, 0x80];
    let mangled = String::from_utf8_lossy(&original).as_bytes().to_vec();
    assert!(
        mangled.len() > original.len(),
        "lossy decoding inflates: {} -> {}",
        original.len(),
        mangled.len()
    );
    assert_ne!(mangled, original, "and does not round-trip");
}
