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

/// The host the request would really go to, by the parser reqwest uses.
fn real_host(url: &str) -> Option<String> {
    reqwest::Url::parse(url).ok()?.host_str().map(String::from)
}

#[test]
fn tokens_only_go_to_the_service() {
    let ok = |url: &str, domain: &str| token_url(url, domain, None).is_some();
    assert!(ok(
        "https://files.slack.com/files-pri/T1-F1/x.pdf",
        "slack.com"
    ));
    assert!(
        ok("https://Files.Slack.com:443/x", "slack.com"),
        "case and default port"
    );
    assert!(ok("https://uploads.linear.app/a/b", "uploads.linear.app"));
    for bad in [
        "http://files.slack.com/x",
        "https://slack.com.evil.dev/x",
        "https://evilslack.com/x",
        "https://files.slack.com@evil.dev/x",
        "https://user@files.slack.com/x",
        "https://files.slack.com:8443/x",
        "https://evil.dev/?u=files.slack.com",
        "https://evil.dev/#files.slack.com",
        "ftp://files.slack.com/x",
        "files.slack.com/x",
        "",
    ] {
        assert!(!ok(bad, "slack.com"), "{bad}");
    }
}

/// Review of #39, high finding 1: a hand-rolled host check read these as
/// Slack's or Linear's host; the WHATWG parser behind reqwest connects to
/// evil.example.
#[test]
fn backslash_tricks_do_not_pass_for_the_service() {
    for (url, domain) in [
        (
            r"https://evil.example\@uploads.linear.app/x",
            "uploads.linear.app",
        ),
        (r"https://evil.example\@files.slack.com/x", "slack.com"),
        (r"https://evil.example\.slack.com/x", "slack.com"),
        (r"https://evil.example\\@files.slack.com/x", "slack.com"),
        ("https://evil.example%5C@files.slack.com/x", "slack.com"),
        ("https://evil.example/\\files.slack.com/x", "slack.com"),
    ] {
        assert_eq!(token_url(url, domain, None), None, "{url}");
    }
    assert_eq!(
        real_host(r"https://evil.example\@uploads.linear.app/x").as_deref(),
        Some("evil.example"),
        "the premise: reqwest goes to evil.example"
    );
}

#[test]
fn the_url_checked_is_the_url_requested() {
    // Whatever passes, its parsed host is the service's, so the request made
    // with the returned Url cannot go elsewhere.
    for url in [
        "https://files.slack.com/files-pri/T1-F1/a%5Cb.pdf",
        "https://files.slack.com/x?next=https://evil.dev",
    ] {
        let u = token_url(url, "slack.com", None).unwrap();
        assert_eq!(u.host_str(), Some("files.slack.com"), "{url}");
    }
}

#[test]
fn the_api_base_origin_is_allowed_and_nothing_near_it() {
    let base = Some("http://127.0.0.1:4000/graphql");
    assert!(token_url("http://127.0.0.1:4000/f", "slack.com", base).is_some());
    assert!(
        token_url("http://127.0.0.1:4001/f", "slack.com", base).is_none(),
        "port"
    );
    assert!(
        token_url("https://127.0.0.1:4000/f", "slack.com", base).is_none(),
        "scheme"
    );
    assert!(
        token_url("http://localhost:4000/f", "slack.com", base).is_none(),
        "host"
    );
}

#[test]
fn an_upload_is_recognised_by_its_parsed_host() {
    assert!(is_https_on(
        "https://uploads.linear.app/o/f/x.png",
        "uploads.linear.app"
    ));
    assert!(!is_https_on(
        r"https://evil.example\@uploads.linear.app/x",
        "uploads.linear.app"
    ));
    assert!(!is_https_on(
        "http://uploads.linear.app/x",
        "uploads.linear.app"
    ));
    assert!(!is_https_on(
        "https://uploads.linear.app.evil.dev/x",
        "uploads.linear.app"
    ));
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

#[test]
fn same_named_files_get_a_suffix_instead_of_overwriting() {
    let names: Vec<String> = [
        "image.png",
        "image.png",
        "Image.PNG",
        "notes",
        "notes",
        "a/b.txt",
        "b.txt",
        "image-2.png",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(
        distinct_names(&names),
        [
            "image.png",
            "image-2.png",
            "Image-3.PNG",
            "notes",
            "notes-2",
            "b.txt",
            "b-2.txt",
            "image-2-2.png"
        ]
    );
}

#[test]
fn an_oversized_upload_is_refused_from_metadata_with_the_service_limit() {
    let root = std::env::temp_dir().join(format!("sidekar-up-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    struct Gone(std::path::PathBuf);
    impl Drop for Gone {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }
    let dir = Gone(root);
    let dir = &dir.0;
    // A sparse 3 GiB file: nothing is written, and nothing must be read.
    let big = dir.join("huge.bin");
    std::fs::File::create(&big)
        .unwrap()
        .set_len(3 << 30)
        .unwrap();
    let big = big.to_str().unwrap();
    let started = std::time::Instant::now();
    for (limit, says) in [
        (SLACK_UPLOAD, "Slack takes files up to 1.0G"),
        (LINEAR_UPLOAD, "Linear takes files up to 2.0G"),
    ] {
        let err = read_upload(big, limit).unwrap_err().to_string();
        assert!(err.contains("is 3.0G") && err.contains(says), "{err}");
        assert!(check_upload(big, limit).is_err());
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "it was read"
    );

    let small = dir.join("a.txt");
    std::fs::write(&small, b"hello").unwrap();
    let small = small.to_str().unwrap();
    let (bytes, name, mime) = read_upload(small, SLACK_UPLOAD).unwrap();
    assert_eq!(
        (bytes.as_slice(), name.as_str(), mime.as_str()),
        (&b"hello"[..], "a.txt", "text/plain")
    );
    let tiny = UploadLimit {
        service: "Test",
        max: 4,
    };
    assert!(
        read_upload(small, tiny)
            .unwrap_err()
            .to_string()
            .contains("Test takes files up to 4B")
    );

    let empty = dir.join("e.txt");
    std::fs::write(&empty, b"").unwrap();
    assert!(
        check_upload(empty.to_str().unwrap(), SLACK_UPLOAD)
            .unwrap_err()
            .to_string()
            .contains("empty")
    );
    assert!(
        check_upload(dir.to_str().unwrap(), SLACK_UPLOAD)
            .unwrap_err()
            .to_string()
            .contains("not a file")
    );
}

#[test]
fn a_shared_file_never_lands_as_a_dotfile() {
    assert_eq!(local_name(".bashrc"), "_bashrc");
    assert_eq!(local_name("../../.ssh/authorized_keys"), "authorized_keys");
    assert_eq!(local_name("x/..npmrc"), "_npmrc");
    assert_eq!(local_name("..."), "attachment");
    assert_eq!(local_name("report.pdf"), "report.pdf");
    assert_eq!(output_path(None, ".zshrc"), PathBuf::from("_zshrc"));
    assert_eq!(
        output_path(Some("d/"), ".profile"),
        PathBuf::from("d/_profile")
    );
    // A path the user typed is theirs to choose.
    assert_eq!(
        output_path(Some(".env.local"), "x"),
        PathBuf::from(".env.local")
    );
}

#[test]
fn a_remote_name_never_overwrites_but_a_typed_path_does() {
    let dir = std::env::temp_dir().join(format!("sidekar-ow-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let into = format!("{}/", dir.display());
    std::fs::write(dir.join("notes.txt"), b"mine").unwrap();

    let p = save(Some(&into), "notes.txt", b"theirs").unwrap();
    assert_eq!(p, dir.join("notes-2.txt"));
    assert_eq!(std::fs::read(dir.join("notes.txt")).unwrap(), b"mine");
    let p = save(Some(&into), "notes.txt", b"again").unwrap();
    assert_eq!(p, dir.join("notes-3.txt"));

    #[cfg(unix)]
    {
        // A symlink in the way is not followed.
        let target = dir.join("target");
        std::fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, dir.join("link.bin")).unwrap();
        let p = save(Some(&into), "link.bin", b"x").unwrap();
        assert_eq!(p, dir.join("link-2.bin"));
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
    }

    let typed = dir.join("notes.txt");
    save(Some(typed.to_str().unwrap()), "whatever", b"asked").unwrap();
    assert_eq!(std::fs::read(&typed).unwrap(), b"asked");
    std::fs::remove_dir_all(dir).ok();
}
