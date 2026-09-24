use super::*;
use serde_json::json;

#[test]
fn header_lookup_is_case_insensitive() {
    let msg = json!({"payload": {"headers": [
        {"name": "From", "value": "a@b.com"},
        {"name": "subject", "value": "hello"}
    ]}});
    assert_eq!(header(&msg, "From"), "a@b.com");
    assert_eq!(header(&msg, "SUBJECT"), "hello");
    assert_eq!(header(&msg, "Cc"), "");
}

fn b64(s: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s.as_bytes())
}

#[test]
fn body_text_reads_a_simple_message() {
    let payload = json!({"body": {"data": b64("plain body")}});
    assert_eq!(body_text(&payload), "plain body");
}

#[test]
fn body_text_prefers_plain_over_html() {
    // multipart/alternative carries the same content twice; the HTML copy is
    // unreadable as text, so the plain part has to win regardless of order.
    let payload = json!({"parts": [
        {"mimeType": "text/html", "body": {"data": b64("<p>rich</p>")}},
        {"mimeType": "text/plain", "body": {"data": b64("plain wins")}}
    ]});
    assert_eq!(body_text(&payload), "plain wins");
}

#[test]
fn body_text_descends_into_nested_parts() {
    let payload = json!({"parts": [
        {"mimeType": "multipart/alternative", "parts": [
            {"mimeType": "text/plain", "body": {"data": b64("buried")}}
        ]}
    ]});
    assert_eq!(body_text(&payload), "buried");
}

#[test]
fn body_text_on_an_attachment_only_message_is_empty_not_garbage() {
    let payload = json!({"parts": [
        {"mimeType": "application/pdf", "body": {"attachmentId": "x"}}
    ]});
    assert_eq!(body_text(&payload), "");
}

#[test]
fn a_line_break_in_a_recipient_is_refused() {
    // Sidekar reads mail, so these values can come from a received message.
    // A CRLF here would end the To: header and start a new one.
    assert!(reject_header_breaks("--to", "a@b.com\r\nBcc: attacker@evil.com").is_err());
    assert!(reject_header_breaks("--to", "a@b.com\nBcc: attacker@evil.com").is_err());
    assert!(reject_header_breaks("--subject", "hi\r\nBcc: attacker@evil.com").is_err());
    assert!(reject_header_breaks("--subject", "hi\0there").is_err());
}

#[test]
fn ordinary_header_values_pass() {
    assert!(reject_header_breaks("--to", "a@b.com").is_ok());
    assert!(reject_header_breaks("--subject", "Q3 invoice, revised").is_ok());
}

#[test]
fn a_plain_subject_is_left_alone() {
    assert_eq!(encode_subject("Q3 invoice"), "Q3 invoice");
}

#[test]
fn a_non_ascii_subject_is_encoded_rather_than_sent_raw() {
    let encoded = encode_subject("Budget café");
    assert!(encoded.starts_with("=?UTF-8?B?"), "got {encoded}");
    assert!(encoded.ends_with("?="));
    let inner = encoded
        .trim_start_matches("=?UTF-8?B?")
        .trim_end_matches("?=");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(inner)
        .unwrap();
    assert_eq!(String::from_utf8(decoded).unwrap(), "Budget café");
}

// ---- the shared message builder -------------------------------------------

#[test]
fn a_draft_is_refused_on_the_same_injection_a_send_is() {
    // The reason `send` and the draft calls share encode_message. A draft is
    // mail that gets sent later, usually by a human who is reading the visible
    // To: line and trusting it — so an injected Bcc: here is worse than in a
    // direct send, not better.
    assert!(encode_message(&compose("a@b.com\r\nBcc: attacker@evil.com", "hi", "body")).is_err());
    assert!(encode_message(&compose("a@b.com", "hi\r\nBcc: attacker@evil.com", "body")).is_err());
    assert!(encode_message(&compose("a@b.com", "hi", "body")).is_ok());
}

#[test]
fn an_encoded_message_is_base64url_and_round_trips() {
    // Gmail rejects the standard alphabet here, and the failure reads as a
    // generic 400, so this is worth pinning.
    let encoded = encode_message(&compose("a@b.com", "Q3", "hello")).unwrap();
    assert!(
        !encoded.contains('+') && !encoded.contains('/') && !encoded.contains('='),
        "expected base64url without padding, got {encoded}"
    );
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&encoded)
        .unwrap();
    let raw = String::from_utf8(decoded).unwrap();
    assert!(raw.starts_with("To: a@b.com\r\nSubject: Q3\r\n"));
    assert!(raw.ends_with("\r\n\r\nhello"));
}

#[test]
fn a_draft_body_is_separated_from_its_headers_by_one_blank_line() {
    // If this collapses, the body is parsed as more headers and the mail
    // arrives empty.
    let raw = String::from_utf8(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encode_message(&compose("a@b.com", "s", "line one\nline two")).unwrap())
            .unwrap(),
    )
    .unwrap();
    let (headers, body) = raw.split_once("\r\n\r\n").expect("no header/body boundary");
    assert!(headers.contains("Content-Type: text/plain; charset=UTF-8"));
    assert_eq!(body, "line one\nline two");
}

/// A minimal Compose, for tests that only care about one field.
fn compose(to: &str, subject: &str, body: &str) -> Compose {
    Compose {
        to: to.to_string(),
        subject: subject.to_string(),
        body: body.to_string(),
        ..Default::default()
    }
}

#[test]
fn cc_and_bcc_are_refused_on_the_same_injection_as_to() {
    // These were the flags that used to be dropped silently. Now that they
    // reach the headers, they are an injection surface like any other.
    let mut m = compose("a@b.com", "hi", "body");
    m.cc = Some("c@b.com\r\nBcc: attacker@evil.com".into());
    assert!(encode_message(&m).is_err());

    let mut m = compose("a@b.com", "hi", "body");
    m.bcc = Some("d@b.com\nTo: attacker@evil.com".into());
    assert!(encode_message(&m).is_err());
}

#[test]
fn cc_and_bcc_appear_only_when_set() {
    let plain = raw_of(&compose("a@b.com", "s", "b"));
    assert!(!plain.contains("Cc:"), "empty Cc should not be emitted");
    assert!(!plain.contains("Bcc:"));

    let mut m = compose("a@b.com", "s", "b");
    m.cc = Some("c@b.com".into());
    m.bcc = Some("d@b.com".into());
    let raw = raw_of(&m);
    assert!(raw.contains("\r\nCc: c@b.com\r\n"));
    assert!(raw.contains("\r\nBcc: d@b.com\r\n"));

    // An explicitly empty value is the same as absent, not a blank header.
    let mut m = compose("a@b.com", "s", "b");
    m.cc = Some(String::new());
    assert!(!raw_of(&m).contains("Cc:"));
}

#[test]
fn a_reply_carries_in_reply_to_and_references() {
    // threadId alone threads it in Gmail's own UI and nowhere else. Every other
    // client reads these two headers, which is why the reply sets all three.
    let mut m = compose("a@b.com", "Re: x", "body");
    m.reply = Some(ReplyContext {
        thread_id: "t1".into(),
        message_id: "<parent@mail>".into(),
        references: "<older@mail> <parent@mail>".into(),
        subject: "x".into(),
    });
    let raw = raw_of(&m);
    assert!(raw.contains("\r\nIn-Reply-To: <parent@mail>\r\n"));
    assert!(raw.contains("\r\nReferences: <older@mail> <parent@mail>\r\n"));
}

#[test]
fn a_parents_headers_are_as_untrusted_as_its_subject() {
    // References and Message-ID come off mail somebody else sent. A raw-MIME
    // reply that trusted them would be injectable by anyone who can email you.
    let mut m = compose("a@b.com", "s", "body");
    m.reply = Some(ReplyContext {
        thread_id: "t1".into(),
        message_id: "<p@mail>\r\nBcc: attacker@evil.com".into(),
        references: String::new(),
        subject: String::new(),
    });
    assert!(encode_message(&m).is_err());
}

#[test]
fn a_reply_subject_gains_one_re_and_no_more() {
    assert_eq!(reply_subject("Invoice"), "Re: Invoice");
    assert_eq!(reply_subject("Re: Invoice"), "Re: Invoice");
    // Gmail and Outlook both treat the prefix case-insensitively.
    assert_eq!(reply_subject("RE: Invoice"), "RE: Invoice");
    assert_eq!(reply_subject("re: Invoice"), "re: Invoice");
    assert_eq!(reply_subject(""), "");
}

fn raw_of(m: &Compose) -> String {
    String::from_utf8(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encode_message(m).unwrap())
            .unwrap(),
    )
    .unwrap()
}

// ---- attachments ----------------------------------------------------------

#[test]
fn attachments_are_found_in_nested_multiparts() {
    // Real mail nests: multipart/mixed wrapping multipart/alternative wrapping
    // the text, with the files as siblings further down.
    let payload = serde_json::json!({
        "mimeType": "multipart/mixed",
        "parts": [
            {"mimeType": "multipart/alternative", "parts": [
                {"mimeType": "text/plain", "filename": "", "body": {"data": ""}},
                {"mimeType": "text/html", "filename": "", "body": {"data": ""}}
            ]},
            {"mimeType": "application/pdf", "filename": "invoice.pdf",
             "body": {"attachmentId": "att1", "size": 1234}},
            {"mimeType": "image/png", "filename": "logo.png",
             "body": {"attachmentId": "att2", "size": 99}}
        ]
    });
    let found = attachments(&payload);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].filename, "invoice.pdf");
    assert_eq!(found[0].size, 1234);
    assert_eq!(found[1].mime, "image/png");
}

#[test]
fn body_parts_are_not_mistaken_for_attachments() {
    // A text part has an empty filename and no attachmentId. Treating those as
    // attachments would offer the reader a file that is just the email body.
    let payload = serde_json::json!({
        "mimeType": "text/plain", "filename": "", "body": {"data": "aGk"}
    });
    assert!(attachments(&payload).is_empty());
}

#[test]
fn a_filename_cannot_escape_the_directory_it_is_written_to() {
    // Attachment names come off received mail, so they are attacker-chosen.
    assert_eq!(safe_filename("../../etc/passwd"), "passwd");
    assert_eq!(safe_filename("/etc/passwd"), "passwd");
    assert_eq!(safe_filename("report.pdf"), "report.pdf");
}

#[test]
fn an_outgoing_filename_cannot_break_its_own_header() {
    // The name goes into Content-Disposition inside quotes; a quote or a CRLF
    // would end the header early, the same way an injected Cc does.
    let dir = std::env::temp_dir().join(format!("sidekar-attach-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bad = dir.join("has\"quote.txt");
    if std::fs::write(&bad, b"x").is_ok() {
        assert!(attachment_from_path(&bad).is_err());
        let _ = std::fs::remove_file(&bad);
    }
    let good = dir.join("fine.txt");
    std::fs::write(&good, b"x").unwrap();
    assert!(attachment_from_path(&good).is_ok());
    let _ = std::fs::remove_file(&good);
}

#[test]
fn a_message_with_attachments_is_multipart_and_keeps_its_bytes() {
    let mut m = compose("a@b.com", "s", "see attached");
    m.attachments = vec![OutgoingAttachment {
        filename: "data.bin".into(),
        mime: "application/octet-stream".into(),
        bytes: vec![0x00, 0xff, 0xfe, 0x80, b'A'],
    }];
    let raw = raw_of(&m);
    assert!(raw.contains("Content-Type: multipart/mixed; boundary="));
    assert!(raw.contains("Content-Disposition: attachment; filename=\"data.bin\""));
    assert!(raw.contains("Content-Transfer-Encoding: base64"));
    assert!(raw.contains("see attached"));
    // The bytes survive as standard-alphabet base64, not the URL-safe one used
    // for the envelope — a client decoding the MIME part would get garbage.
    let expected = base64::engine::general_purpose::STANDARD.encode([0x00, 0xff, 0xfe, 0x80, b'A']);
    assert!(raw.contains(&expected), "expected {expected} in the part");
}

#[test]
fn base64_parts_are_wrapped_for_transport() {
    // Unwrapped base64 produces one enormous header-less line; some relays fold
    // or truncate it, and the attachment arrives corrupt.
    let wrapped = base64_mime(&vec![b'x'; 1000]);
    assert!(wrapped.contains("\r\n"));
    assert!(
        wrapped.split("\r\n").all(|l| l.len() <= 76),
        "a line exceeded 76 columns"
    );
}

#[test]
fn an_oversized_message_is_refused_before_it_is_sent() {
    // Gmail answers a bare 413 that never mentions attachments, so this is
    // caught locally where the error can name the cause.
    let mut m = compose("a@b.com", "s", "body");
    m.attachments = vec![OutgoingAttachment {
        filename: "big.bin".into(),
        mime: "application/octet-stream".into(),
        bytes: vec![0u8; MAX_RAW_BYTES],
    }];
    let err = encode_message(&m).unwrap_err().to_string();
    assert!(err.contains("over Gmail's"), "unhelpful error: {err}");
    assert!(
        err.contains("drive put"),
        "should point at the way around it"
    );
}

#[test]
fn mime_types_come_from_the_extension() {
    assert_eq!(mime_for("a.pdf"), "application/pdf");
    assert_eq!(mime_for("A.PDF"), "application/pdf");
    assert_eq!(mime_for("photo.jpeg"), "image/jpeg");
    // Unknown and extensionless both fall back to the type that makes clients
    // offer to save rather than try to render.
    assert_eq!(mime_for("thing.qqq"), "application/octet-stream");
    assert_eq!(mime_for("Makefile"), "application/octet-stream");
}
