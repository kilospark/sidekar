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
