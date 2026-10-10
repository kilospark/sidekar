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
    assert_eq!(m.files, vec!["log.txt"]);
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

#[test]
fn a_ts_reads_as_a_date() {
    assert_eq!(ts_to_date("86400.000100"), "1970-01-02 00:00:00 UTC");
    assert_eq!(ts_to_date("garbage"), "garbage");
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
    let server = MockServer::sequence(vec![json!({"ok": true, "channels": [
        {"id": "C0000000B", "name": "eng-alerts"}
    ]})]);
    let err = resolve_channel(&slack_at(&server), "eng")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("#eng-alerts"), "{err}");
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
