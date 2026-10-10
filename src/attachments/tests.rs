use super::*;

#[test]
fn sizes_read_like_gmail_attachments() {
    assert_eq!(human_bytes(512), "512B");
    assert_eq!(human_bytes(1536), "1.5K");
    assert_eq!(human_bytes(3 * 1024 * 1024), "3.0M");
}

#[test]
fn downloads_land_by_name_or_inside_a_directory() {
    assert_eq!(
        output_path(None, "../../etc/passwd"),
        PathBuf::from("passwd")
    );
    assert_eq!(
        output_path(Some("out/"), "a/b.pdf"),
        PathBuf::from("out/b.pdf")
    );
    assert_eq!(output_path(Some("x.pdf"), "b.pdf"), PathBuf::from("x.pdf"));
    let dir = std::env::temp_dir();
    assert_eq!(
        output_path(Some(dir.to_str().unwrap()), "b.pdf"),
        dir.join("b.pdf")
    );
}

#[test]
fn save_creates_the_directory() {
    let dir = std::env::temp_dir().join(format!("sidekar-att-{}", std::process::id()));
    let out = format!("{}/", dir.display());
    let p = save(Some(&out), "note.txt", b"hi").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"hi");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn binary_is_not_printed() {
    assert_eq!(printable("a.txt", b"hello").unwrap(), "hello");
    assert!(
        printable("a.png", &[0x89, b'P', 0, 1])
            .unwrap_err()
            .to_string()
            .contains("--out")
    );
}

#[test]
fn tokens_only_go_to_the_service() {
    assert!(token_may_go_to(
        "https://files.slack.com/files-pri/T1-F1/x.pdf",
        "slack.com",
        None
    ));
    assert!(token_may_go_to(
        "https://uploads.linear.app/a/b",
        "uploads.linear.app",
        None
    ));
    assert!(
        !token_may_go_to("http://files.slack.com/x", "slack.com", None),
        "https only"
    );
    assert!(!token_may_go_to(
        "https://slack.com.evil.dev/x",
        "slack.com",
        None
    ));
    assert!(!token_may_go_to(
        "https://evilslack.com/x",
        "slack.com",
        None
    ));
    assert!(!token_may_go_to(
        "https://files.slack.com@evil.dev/x",
        "slack.com",
        None
    ));
    assert!(!token_may_go_to(
        "https://evil.dev/?u=files.slack.com",
        "slack.com",
        None
    ));
    assert!(token_may_go_to(
        "http://127.0.0.1:4000/f",
        "slack.com",
        Some("127.0.0.1")
    ));
}

#[test]
fn hosts_parse() {
    assert_eq!(
        host_of("https://A.Slack.com:443/x").as_deref(),
        Some("a.slack.com")
    );
    assert_eq!(host_of("https://u:p@h.io/x").as_deref(), Some("h.io"));
    assert_eq!(host_of("ftp://h.io"), None);
}

#[test]
fn sentence_punctuation_is_not_part_of_a_link() {
    for (found, link) in [
        ("https://x.dev/a.", "https://x.dev/a"),
        ("https://x.dev/a,", "https://x.dev/a"),
        ("https://x.dev/a?!", "https://x.dev/a"),
        ("https://x.dev/a).", "https://x.dev/a"),
        ("https://x.dev/a\"", "https://x.dev/a"),
        ("https://x.dev/a?b=1", "https://x.dev/a?b=1"),
        ("https://x.dev/a.png", "https://x.dev/a.png"),
        (
            "https://en.wikipedia.org/wiki/Rust_(language)",
            "https://en.wikipedia.org/wiki/Rust_(language)",
        ),
        (
            "https://en.wikipedia.org/wiki/Rust_(language)).",
            "https://en.wikipedia.org/wiki/Rust_(language)",
        ),
        ("https://x.dev/a/", "https://x.dev/a/"),
    ] {
        assert_eq!(trim_link_end(found), link, "{found}");
    }
}
