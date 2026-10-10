use super::*;
use crate::test_http::MockServer;
use serde_json::json;

fn slack_at(server: &MockServer) -> Slack {
    Slack::with_base(MockServer::client(), &server.base, "xoxp-test".into())
}

#[test]
fn markup_becomes_readable_text() {
    let mut names = HashMap::new();
    names.insert("U1".to_string(), "Alice".to_string());
    let raw = "hey <@U1> and <@U2|bob>, see <#C1|general> and <https://x.dev|the docs> \
               or <https://y.dev>. <!here> &lt;b&gt; &amp; <mailto:a@b.co|a@b.co>";
    assert_eq!(
        render_text(raw, &names),
        "hey @Alice and @bob, see #general and the docs (https://x.dev) or https://y.dev. \
         @here <b> & a@b.co"
    );
}

#[test]
fn markup_with_no_closing_bracket_is_left_alone() {
    assert_eq!(render_text("a < b", &HashMap::new()), "a < b");
    assert_eq!(
        render_text("<!subteam^S1|@eng> go", &HashMap::new()),
        "@eng go"
    );
}

#[test]
fn mentions_and_authors_are_collected_once() {
    let msgs = vec![
        message_from(&json!({"ts": "1.0", "user": "U1", "text": "hi <@U2> <@U1>"})),
        message_from(&json!({"ts": "2.0", "user": "U2", "text": "<@U3|carol>"})),
    ];
    assert_eq!(user_ids_in(&msgs), vec!["U1", "U2", "U3"]);
}

#[test]
fn ids_are_told_apart_from_names() {
    assert!(looks_like_conversation_id("C0123ABCD"));
    assert!(looks_like_conversation_id("D0123ABCD"));
    // Channel names are lower-case; a short or mixed one is a name.
    assert!(!looks_like_conversation_id("general"));
    assert!(!looks_like_conversation_id("C01"));
    assert!(looks_like_user_id("U0123ABCD"));
    assert!(!looks_like_user_id("C0123ABCD"));
}

#[test]
fn a_message_link_gives_channel_ts_and_thread() {
    let (c, ts, thread) =
        parse_permalink("https://ks.slack.com/archives/C0123ABCD/p1700000000123456").unwrap();
    assert_eq!(
        (c.as_str(), ts.as_str(), thread),
        ("C0123ABCD", "1700000000.123456", None)
    );
    let (_, ts, thread) = parse_permalink(
        "https://ks.slack.com/archives/C0123ABCD/p1700000001000200?thread_ts=1700000000.123456&cid=C0123ABCD",
    )
    .unwrap();
    assert_eq!(ts, "1700000001.000200");
    assert_eq!(thread.as_deref(), Some("1700000000.123456"));
    assert!(parse_permalink("https://example.com/x").is_none());
    assert!(parse_permalink("https://ks.slack.com/archives/general/p1").is_none());
}

#[test]
fn conversation_types_accept_short_and_slack_names() {
    assert_eq!(
        conversation_types("public,private").unwrap(),
        "public_channel,private_channel"
    );
    assert_eq!(conversation_types("dm,mpim").unwrap(), "im,mpim");
    assert_eq!(
        conversation_types("public,all").unwrap(),
        "public_channel,private_channel,im,mpim"
    );
    assert!(conversation_types("bogus").is_err());
    assert!(conversation_types("").is_err());
}

#[test]
fn channel_kinds_are_read_from_their_flags() {
    assert_eq!(
        channel_from(&json!({"id": "C1", "name": "g"})).kind,
        "public"
    );
    assert_eq!(
        channel_from(&json!({"id": "G1", "is_private": true})).kind,
        "private"
    );
    let dm = channel_from(&json!({"id": "D1", "is_im": true, "user": "U9"}));
    assert_eq!(
        (dm.kind, dm.user.as_deref(), dm.is_member),
        ("dm", Some("U9"), true)
    );
    assert_eq!(
        channel_from(&json!({"id": "G2", "is_mpim": true, "is_private": true})).kind,
        "group-dm"
    );
}

#[test]
fn a_bot_message_is_named_by_its_profile() {
    let m = message_from(
        &json!({"ts": "1.0", "bot_id": "B1", "bot_profile": {"name": "CI"},
        "text": "build passed", "files": [{"name": "log.txt"}]}),
    );
    assert_eq!(m.username, "CI");
    assert_eq!(m.files[0].name, "log.txt");
}

#[test]
fn users_answer_to_handle_display_or_real_name() {
    let u = user_from(&json!({"id": "U1", "name": "kb", "real_name": "K B",
        "profile": {"display_name": "kay", "email": "k@b.co"}}));
    assert!(user_matches(&u, "@kb"));
    assert!(user_matches(&u, "Kay"));
    assert!(user_matches(&u, "k b"));
    assert!(!user_matches(&u, "k"));
    assert_eq!(u.label(), "kay");
    assert_eq!(u.email, "k@b.co");
}

#[tokio::test]
async fn history_pages_until_the_limit_and_reads_oldest_first() {
    let server = MockServer::sequence(vec![
        json!({"ok": true, "messages": [{"ts": "3.0", "text": "c"}, {"ts": "2.0", "text": "b"}],
               "response_metadata": {"next_cursor": "cur2"}}),
        json!({"ok": true, "messages": [{"ts": "1.0", "text": "a"}, {"ts": "0.5", "text": "z"}],
               "response_metadata": {"next_cursor": "cur3"}}),
    ]);
    let msgs = history(&slack_at(&server), "C0123ABCD", 3, None)
        .await
        .unwrap();
    let texts: Vec<&str> = msgs.iter().map(|m| m.text.as_str()).collect();
    assert_eq!(texts, vec!["a", "b", "c"]);
    let reqs = server.requests();
    assert_eq!(reqs.len(), 2, "stops once it has enough");
    assert_eq!(reqs[0].path(), "/conversations.history");
    assert_eq!(reqs[0].query()["channel"], "C0123ABCD");
    assert_eq!(reqs[1].query()["cursor"], "cur2");
}

#[tokio::test]
async fn posting_into_a_thread_sends_thread_ts() {
    let server = MockServer::sequence(vec![json!({"ok": true, "channel": "C1", "ts": "9.9"})]);
    let (c, ts) = post(&slack_at(&server), "C1", "done", Some("1.5"), true)
        .await
        .unwrap();
    assert_eq!((c.as_str(), ts.as_str()), ("C1", "9.9"));
    let body = server.requests()[0].json();
    assert_eq!(body["thread_ts"], "1.5");
    assert_eq!(body["reply_broadcast"], true);
    assert_eq!(body["text"], "done");
}

#[tokio::test]
async fn a_channel_name_resolves_by_listing() {
    let server = MockServer::sequence(vec![json!({"ok": true, "channels": [
        {"id": "C0000000A", "name": "general"}, {"id": "C0000000B", "name": "eng-alerts"}
    ]})]);
    let slack = slack_at(&server);
    assert_eq!(
        resolve_channel(&slack, "#eng-alerts").await.unwrap(),
        "C0000000B"
    );
    // An id needs no lookup at all.
    assert_eq!(
        resolve_channel(&slack, "C0123ABCD").await.unwrap(),
        "C0123ABCD"
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn an_unknown_channel_suggests_near_names() {
    let server = MockServer::sequence(vec![
        json!({"ok": true, "channels": [{"id": "C0000000C", "name": "eng-oncall"}]}),
        json!({"ok": true, "channels": [{"id": "C0000000B", "name": "eng-alerts"}]}),
    ]);
    let err = resolve_channel(&slack_at(&server), "eng")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("#eng-alerts") && err.contains("#eng-oncall"),
        "{err}"
    );
    let paths: Vec<String> = server
        .requests()
        .iter()
        .map(|r| r.path().to_string())
        .collect();
    assert_eq!(paths, ["/users.conversations", "/conversations.list"]);
}

#[tokio::test]
async fn a_person_resolves_to_a_dm() {
    let server = MockServer::sequence(vec![
        json!({"ok": true, "user": {"id": "U0000000A", "name": "kb"}}),
        json!({"ok": true, "channel": {"id": "D0000000A"}}),
    ]);
    let id = resolve_channel(&slack_at(&server), "kb@example.com")
        .await
        .unwrap();
    assert_eq!(id, "D0000000A");
    let reqs = server.requests();
    assert_eq!(reqs[0].path(), "/users.lookupByEmail");
    assert_eq!(reqs[1].path(), "/conversations.open");
    assert_eq!(reqs[1].json()["users"], "U0000000A");
}

#[tokio::test]
async fn an_ambiguous_name_is_refused() {
    let server = MockServer::sequence(vec![json!({"ok": true, "members": [
        {"id": "U1", "name": "sam1", "real_name": "Sam"},
        {"id": "U2", "name": "sam2", "real_name": "Sam"}
    ]})]);
    let err = resolve_user(&slack_at(&server), "sam")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("several"), "{err}");
}

#[tokio::test]
async fn search_reads_matches() {
    let server = MockServer::sequence(vec![json!({"ok": true, "messages": {"matches": [
        {"channel": {"id": "C1", "name": "eng"}, "ts": "1.0", "user": "U1",
         "username": "kb", "text": "deploy", "permalink": "https://x"}
    ]}})]);
    let found = search(&slack_at(&server), "deploy in:#eng", 5)
        .await
        .unwrap();
    assert_eq!(found[0].channel_name, "eng");
    assert_eq!(found[0].permalink, "https://x");
    let q = server.requests()[0].query();
    assert_eq!(q["query"], "deploy in:#eng");
    assert_eq!(q["count"], "5");
}

#[test]
fn draft_text_becomes_one_rich_text_block_with_links() {
    let b = text_to_blocks("see https://x.dev/a?b=1 and http://y.io now");
    let els = &b[0]["elements"][0]["elements"];
    assert_eq!(b[0]["type"], "rich_text");
    assert_eq!(els[0], json!({"type": "text", "text": "see "}));
    assert_eq!(
        els[1],
        json!({"type": "link", "url": "https://x.dev/a?b=1"})
    );
    assert_eq!(els[2], json!({"type": "text", "text": " and "}));
    assert_eq!(els[3], json!({"type": "link", "url": "http://y.io"}));
    assert_eq!(els[4], json!({"type": "text", "text": " now"}));
    let plain = text_to_blocks("multi\nline");
    assert_eq!(
        plain[0]["elements"][0]["elements"][0]["text"],
        "multi\nline"
    );
}

#[test]
fn client_msg_ids_are_v4_uuids() {
    let a = uuid_v4();
    assert_eq!(a.len(), 36);
    assert_eq!(&a[14..15], "4");
    assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"));
    assert_ne!(a, uuid_v4());
}

#[tokio::test]
async fn a_draft_goes_to_drafts_create_and_is_not_posted() {
    let server = MockServer::sequence(vec![
        json!({"ok": true, "draft": {"id": "Dr01", "team_id": "T1"}}),
    ]);
    let id = draft_create(&slack_at(&server), "C0000000A", "hi", Some("1.5"))
        .await
        .unwrap();
    assert_eq!(id, "Dr01");
    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].path(), "/drafts.create", "never chat.postMessage");
    let body = reqs[0].json();
    assert_eq!(
        body["destinations"],
        json!([{"channel_id": "C0000000A", "thread_ts": "1.5"}])
    );
    assert_eq!(body["file_ids"], json!([]));
    assert_eq!(
        body["blocks"][0]["elements"][0]["elements"][0]["text"],
        "hi"
    );
}

#[tokio::test]
async fn a_composer_that_already_has_a_draft_says_so() {
    let server = MockServer::sequence(vec![json!({"ok": false, "error": "attached_draft_exists"})]);
    let err = draft_create(&slack_at(&server), "C0000000A", "hi", None)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("already holds a draft"), "{err}");
}

#[tokio::test]
async fn bookmarks_are_read_for_a_channel() {
    let server = MockServer::sequence(vec![json!({"ok": true, "bookmarks": [
        {"id": "Bk1", "title": "Runbook", "link": "https://wiki/run", "type": "link", "emoji": ":book:"}
    ]})]);
    let found = bookmarks(&slack_at(&server), "C0000000A").await.unwrap();
    assert_eq!(found[0].title, "Runbook");
    assert_eq!(found[0].link, "https://wiki/run");
    let req = &server.requests()[0];
    assert_eq!(req.path(), "/bookmarks.list");
    assert_eq!(req.query()["channel_id"], "C0000000A");
}

fn routed(
    route: impl Fn(&str, &crate::test_http::Request) -> (String, Vec<u8>) + Send + Sync + 'static,
) -> MockServer {
    MockServer::start_raw(move |req| {
        let (ct, body) = route(req.path(), req);
        (200, ct, body)
    })
}

fn json_reply(v: Value) -> (String, Vec<u8>) {
    ("application/json".into(), v.to_string().into_bytes())
}

#[test]
fn messages_carry_file_details() {
    let m = message_from(&json!({"ts": "1.0", "files": [
        {"id": "F0ABCDEF1", "name": "report.pdf", "mimetype": "application/pdf", "filetype": "pdf",
         "size": 1536, "mode": "hosted", "url_private_download": "https://files.slack.com/d/report.pdf",
         "url_private": "https://files.slack.com/p/report.pdf", "channels": ["C1"], "ims": ["D1"]},
        {"id": "F0DELETED1", "mode": "tombstone"}
    ]}));
    let f = &m.files[0];
    assert_eq!(
        f.url, "https://files.slack.com/d/report.pdf",
        "download URL preferred"
    );
    assert_eq!(f.channels, ["C1", "D1"]);
    assert_eq!(f.summary(), "F0ABCDEF1 report.pdf (application/pdf, 1.5K)");
    assert_eq!(m.files[1].summary(), "F0DELETED1 (deleted)");
    let ext = file_from(
        &json!({"id": "F0EXTERNAL", "title": "Plan", "filetype": "gdoc", "mode": "external"}),
    );
    assert_eq!(ext.summary(), "F0EXTERNAL Plan (gdoc, external)");
}

#[test]
fn file_ids_come_from_ids_and_links() {
    assert_eq!(parse_file_id("F0ABCDEF12").as_deref(), Some("F0ABCDEF12"));
    assert_eq!(
        parse_file_id("https://acme.slack.com/files/U012345678/F0ABCDEF12/report.pdf").as_deref(),
        Some("F0ABCDEF12")
    );
    assert_eq!(
        parse_file_id("https://files.slack.com/files-pri/T0123456-F0ABCDEF12/report.pdf")
            .as_deref(),
        Some("F0ABCDEF12")
    );
    assert_eq!(parse_file_id("C0ABCDEF12"), None);
    assert_eq!(parse_file_id("report.pdf"), None);
}

#[tokio::test]
async fn a_file_downloads_with_the_token_and_checks_its_size() {
    let server = routed(|path, _| match path {
        "/files.info" => json_reply(
            json!({"ok": true, "file": {"id": "F0ABCDEF12", "name": "a.bin",
            "mimetype": "application/octet-stream", "size": 4, "mode": "hosted",
            "url_private_download": "BASE/dl/a.bin"}}),
        ),
        "/dl/a.bin" => ("application/octet-stream".into(), vec![0, 1, 2, 3]),
        _ => json_reply(json!({"ok": false, "error": "unexpected"})),
    });
    let slack = slack_at(&server);
    let mut f = file_info(&slack, "F0ABCDEF12").await.unwrap();
    f.url = f.url.replace("BASE", &server.base);
    assert_eq!(file_download(&slack, &f).await.unwrap(), vec![0, 1, 2, 3]);
    let reqs = server.requests();
    assert_eq!(reqs[0].query()["file"], "F0ABCDEF12");
    assert_eq!(reqs[1].headers["authorization"], "Bearer xoxp-test");
    f.size = 99;
    assert!(
        file_download(&slack, &f)
            .await
            .unwrap_err()
            .to_string()
            .contains("partial")
    );
}

#[tokio::test]
async fn a_sign_in_page_instead_of_the_file_means_files_read_is_missing() {
    let server = routed(|_, _| {
        (
            "text/html; charset=utf-8".into(),
            b"<html>sign in</html>".to_vec(),
        )
    });
    let slack = slack_at(&server);
    let f = SlackFile {
        id: "F1".into(),
        mimetype: "application/pdf".into(),
        url: format!("{}/x.pdf", server.base),
        ..Default::default()
    };
    let err = file_download(&slack, &f).await.unwrap_err().to_string();
    assert!(err.contains("files:read"), "{err}");
}

#[tokio::test]
async fn the_token_is_never_sent_off_slack() {
    let server = routed(|_, _| ("text/plain".into(), b"x".to_vec()));
    let slack = slack_at(&server);
    let f = SlackFile {
        id: "F1".into(),
        url: "https://evil.example/steal".into(),
        ..Default::default()
    };
    assert!(
        file_download(&slack, &f)
            .await
            .unwrap_err()
            .to_string()
            .contains("refusing")
    );
    assert!(server.requests().is_empty());
    let ext = SlackFile {
        id: "F2".into(),
        mode: "external".into(),
        external_url: "https://drive/x".into(),
        ..Default::default()
    };
    assert!(
        file_download(&slack, &ext)
            .await
            .unwrap_err()
            .to_string()
            .contains("https://drive/x")
    );
}

#[tokio::test]
async fn uploads_use_the_external_flow_and_share_once() {
    let base = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let b2 = base.clone();
    let server = routed(move |path, req| match path {
        "/files.getUploadURLExternal" => {
            let name = req.form()["filename"].clone();
            json_reply(
                json!({"ok": true, "upload_url": format!("{}/up/{name}", b2.lock().unwrap()),
                "file_id": format!("F_{name}")}),
            )
        }
        p if p.starts_with("/up/") => ("text/plain".into(), b"OK - 5".to_vec()),
        "/files.completeUploadExternal" => json_reply(json!({"ok": true, "files": [
            {"id": "F_a.txt", "permalink": "https://x.slack.com/files/U1/F_a.txt/a.txt"},
            {"id": "F_b.txt", "permalink": "p2"}]})),
        _ => json_reply(json!({"ok": false, "error": "unexpected"})),
    });
    *base.lock().unwrap() = server.base.clone();
    let files = vec![
        Upload {
            name: "a.txt".into(),
            title: None,
            bytes: b"hello".to_vec(),
        },
        Upload {
            name: "b.txt".into(),
            title: Some("Bee".into()),
            bytes: b"bz".to_vec(),
        },
    ];
    let out = upload_files(
        &slack_at(&server),
        "C0000000A",
        files,
        Some("see attached"),
        Some("1.5"),
    )
    .await
    .unwrap();
    assert_eq!(out.len(), 2);
    let reqs = server.requests();
    let paths: Vec<&str> = reqs.iter().map(|r| r.path()).collect();
    assert_eq!(
        paths,
        [
            "/files.getUploadURLExternal",
            "/up/a.txt",
            "/files.getUploadURLExternal",
            "/up/b.txt",
            "/files.completeUploadExternal"
        ]
    );
    assert_eq!(reqs[0].form()["length"], "5");
    assert_eq!(reqs[1].body, "hello");
    assert!(
        !reqs[1].headers.contains_key("authorization"),
        "the signed URL needs no token"
    );
    let done = reqs[4].form();
    assert_eq!(done["channel_id"], "C0000000A");
    assert_eq!(done["thread_ts"], "1.5");
    assert_eq!(done["initial_comment"], "see attached");
    let listed: Value = serde_json::from_str(&done["files"]).unwrap();
    assert_eq!(
        listed,
        json!([{"id": "F_a.txt", "title": "a.txt"}, {"id": "F_b.txt", "title": "Bee"}])
    );
    assert!(
        !paths.contains(&"/files.upload"),
        "the retired method is never used"
    );
}

#[tokio::test]
async fn a_channel_lookup_stops_at_the_first_exact_match() {
    // Page one of the person's own channels has it; the next page and the
    // workspace-wide list are never fetched.
    let server = MockServer::sequence(vec![json!({"ok": true,
        "channels": [{"id": "C0000000A", "name": "general"}],
        "response_metadata": {"next_cursor": "more"}})]);
    assert_eq!(
        resolve_channel(&slack_at(&server), "#general")
            .await
            .unwrap(),
        "C0000000A"
    );
    assert_eq!(server.requests().len(), 1);
    assert_eq!(server.requests()[0].path(), "/users.conversations");
}

#[tokio::test]
async fn resolved_names_are_cached_for_a_while() {
    let _home = crate::ScratchHome::new();
    let cache = crate::slack::name_cache_path("T_TEST");
    let server = MockServer::sequence(vec![
        json!({"ok": true, "channels": [{"id": "C0000000A", "name": "general"}]}),
        json!({"ok": true, "channel": {"id": "C0000000A", "name": "general"}}),
    ]);
    let slack = slack_at(&server).with_cache(Some(cache.clone()));
    assert_eq!(
        resolve_channel(&slack, "general").await.unwrap(),
        "C0000000A"
    );
    assert_eq!(
        resolve_channel(&slack, "#General").await.unwrap(),
        "C0000000A"
    );
    let paths: Vec<String> = server
        .requests()
        .iter()
        .map(|r| r.path().to_string())
        .collect();
    assert_eq!(
        paths,
        ["/users.conversations", "/conversations.info"],
        "second lookup came from the cache, checked with one call"
    );
    // An entry past its TTL is ignored.
    let stale = json!({"channel:general": {"id": "COLD",
        "at": crate::oauth_loopback::now_secs() - crate::slack::NAME_CACHE_TTL_SECS - 1}});
    std::fs::write(&cache, stale.to_string()).unwrap();
    assert_eq!(slack.cache_get("channel:general"), None);
}

#[tokio::test]
async fn a_handle_match_stops_the_user_walk_but_a_shared_name_does_not() {
    let page1 = json!({"ok": true, "members": [
        {"id": "U0000000A", "name": "alex", "profile": {"display_name": "Alex"}}
    ], "response_metadata": {"next_cursor": "p2"}});
    let server = MockServer::sequence(vec![page1.clone()]);
    let u = resolve_user(&slack_at(&server), "@alex").await.unwrap();
    assert_eq!(u.id, "U0000000A");
    assert_eq!(server.requests().len(), 1, "a handle is unique");

    let page1 = json!({"ok": true, "members": [
        {"id": "U0000000B", "name": "a.one", "profile": {"display_name": "Sam"}}
    ], "response_metadata": {"next_cursor": "p2"}});
    let page2 = json!({"ok": true, "members": [
        {"id": "U0000000C", "name": "a.two", "profile": {"display_name": "Sam"}}
    ]});
    let server = MockServer::sequence(vec![page1, page2]);
    let err = resolve_user(&slack_at(&server), "Sam")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("several"), "{err}");
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn names_are_looked_up_concurrently_and_once_each() {
    let server = MockServer::start_concurrent(|req| {
        std::thread::sleep(std::time::Duration::from_millis(150));
        let id = req.query()["user"].clone();
        (
            200,
            json!({"ok": true, "user": {"id": id, "name": format!("n-{id}")}}).to_string(),
        )
    });
    let ids: Vec<String> = (0..8).map(|i| format!("U00000000{i}")).collect();
    let mut with_dupes = ids.clone();
    with_dupes.extend(ids.iter().cloned());
    let started = std::time::Instant::now();
    let names = names_for(&slack_at(&server), &with_dupes).await;
    assert_eq!(names.len(), 8);
    assert_eq!(server.requests().len(), 8, "one call per distinct id");
    assert!(
        started.elapsed() < std::time::Duration::from_millis(150 * 8),
        "ran one after another: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_link_at_the_end_of_a_sentence_leaves_the_full_stop_as_text() {
    let b = text_to_blocks("Spec: https://x.dev/spec. Also (https://y.io/a), ok? https://");
    let els = b[0]["elements"][0]["elements"].as_array().unwrap();
    let links: Vec<&str> = els
        .iter()
        .filter(|e| e["type"] == "link")
        .map(|e| e["url"].as_str().unwrap())
        .collect();
    assert_eq!(links, ["https://x.dev/spec", "https://y.io/a"]);
    let text: String = els
        .iter()
        .map(|e| e["text"].as_str().or(e["url"].as_str()).unwrap())
        .collect();
    assert_eq!(
        text, "Spec: https://x.dev/spec. Also (https://y.io/a), ok? https://",
        "nothing lost or doubled"
    );
}

#[tokio::test]
async fn a_backslash_url_never_gets_the_slack_token() {
    let server = MockServer::sequence(vec![]);
    let slack = slack_at(&server);
    let (port, hits) = crate::test_http::decoy();
    for url in [
        format!(r"https://127.0.0.1:{port}\@files.slack.com/x"),
        format!(r"https://127.0.0.1:{port}\.slack.com/x"),
    ] {
        let err = slack.download(&url, false).await.unwrap_err().to_string();
        assert!(err.contains("refusing to send the Slack token"), "{err}");
    }
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_cached_channel_that_was_renamed_is_looked_up_again() {
    let _home = crate::ScratchHome::new();
    let cache = crate::slack::name_cache_path("T_RENAME");
    let fresh = json!({"channel:general": {"id": "COLD00001",
        "at": crate::oauth_loopback::now_secs()}});
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    std::fs::write(&cache, fresh.to_string()).unwrap();
    let server = MockServer::sequence(vec![
        // The cached channel is now called something else.
        json!({"ok": true, "channel": {"id": "COLD00001", "name": "general-old"}}),
        json!({"ok": true, "channels": [{"id": "CNEW00001", "name": "general"}]}),
    ]);
    let slack = slack_at(&server).with_cache(Some(cache.clone()));
    assert_eq!(
        resolve_channel(&slack, "#general").await.unwrap(),
        "CNEW00001"
    );
    assert_eq!(
        slack.cache_get("channel:general").as_deref(),
        Some("CNEW00001")
    );
}

#[tokio::test]
async fn only_handles_are_cached_and_a_hit_must_still_be_that_handle() {
    let _home = crate::ScratchHome::new();
    let cache = crate::slack::name_cache_path("T_USERS");
    let alex = json!({"id": "U0000000A", "name": "alex", "profile": {"display_name": "Al"}});
    let server = MockServer::sequence(vec![
        json!({"ok": true, "members": [alex.clone()]}),
        // Display name: scanned again, not cached.
        json!({"ok": true, "members": [alex.clone()]}),
        json!({"ok": true, "members": [alex.clone()]}),
    ]);
    let slack = slack_at(&server).with_cache(Some(cache.clone()));
    assert_eq!(resolve_user(&slack, "@alex").await.unwrap().id, "U0000000A");
    assert_eq!(slack.cache_get("user:alex").as_deref(), Some("U0000000A"));
    resolve_user(&slack, "Al").await.unwrap();
    resolve_user(&slack, "Al").await.unwrap();
    assert_eq!(slack.cache_get("user:al"), None);
    assert_eq!(server.requests().len(), 3);

    // The cached id now has another handle: scan again, find the new owner.
    let server = MockServer::sequence(vec![
        json!({"ok": true, "user": {"id": "U0000000A", "name": "alex.old"}}),
        json!({"ok": true, "members": [{"id": "U0000000B", "name": "alex"}]}),
    ]);
    let slack = slack_at(&server).with_cache(Some(cache));
    assert_eq!(resolve_user(&slack, "@alex").await.unwrap().id, "U0000000B");
}
