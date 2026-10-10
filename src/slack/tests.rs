use super::*;
use crate::test_http::MockServer;
use serde_json::json;

#[test]
fn ok_true_is_returned_as_is() {
    let v = check(
        "auth.test",
        reqwest::StatusCode::OK,
        r#"{"ok":true,"user":"kb"}"#,
    )
    .unwrap();
    assert_eq!(v["user"], "kb");
}

#[test]
fn ok_false_names_the_method_and_the_error() {
    let err = check(
        "conversations.history",
        reqwest::StatusCode::OK,
        r#"{"ok":false,"error":"channel_not_found"}"#,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("conversations.history"), "{err}");
    assert!(err.contains("channel_not_found"), "{err}");
}

#[test]
fn a_missing_scope_says_which_one() {
    let v = json!({"ok": false, "error": "missing_scope", "needed": "search:read", "provided": "chat:write"});
    let msg = explain_error("missing_scope", &v);
    assert!(msg.contains("search:read"), "{msg}");
    assert!(msg.contains("chat:write"), "{msg}");
    assert!(msg.contains("slack login"), "{msg}");
}

#[test]
fn a_revoked_token_points_at_login() {
    let msg = explain_error("token_revoked", &json!({}));
    assert!(msg.contains("slack login"), "{msg}");
}

#[test]
fn a_non_json_body_still_reports_the_status() {
    let err = check(
        "x",
        reqwest::StatusCode::BAD_GATEWAY,
        "<html>bad gateway</html>",
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("502"), "{err}");
}

#[tokio::test]
async fn get_sends_the_token_and_query_and_post_sends_json() {
    let server = MockServer::start(|_| (200, json!({"ok": true}).to_string()));
    let slack = Slack::with_base(MockServer::client(), &server.base, "xoxp-test".into());
    slack
        .get("conversations.list", &[("limit", "5".into())])
        .await
        .unwrap();
    slack
        .post("chat.postMessage", &json!({"channel": "C1", "text": "hi"}))
        .await
        .unwrap();
    let reqs = server.requests();
    assert_eq!(reqs[0].method, "GET");
    assert_eq!(reqs[0].path(), "/conversations.list");
    assert_eq!(reqs[0].query()["limit"], "5");
    assert_eq!(reqs[0].headers["authorization"], "Bearer xoxp-test");
    assert_eq!(reqs[1].method, "POST");
    assert!(reqs[1].headers["content-type"].starts_with("application/json"));
    assert_eq!(reqs[1].json()["text"], "hi");
}

#[tokio::test]
async fn a_rate_limited_call_is_retried_after_the_stated_wait() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let n = std::sync::Arc::new(AtomicUsize::new(0));
    let seen = n.clone();
    let server = MockServer::start(move |_| {
        if seen.fetch_add(1, Ordering::SeqCst) == 0 {
            (
                429,
                json!({"ok": false, "error": "ratelimited"}).to_string(),
            )
        } else {
            (200, json!({"ok": true, "n": 2}).to_string())
        }
    });
    let slack = Slack::with_base(MockServer::client(), &server.base, "t".into());
    let v = slack.get("users.list", &[]).await.unwrap();
    assert_eq!(v["n"], 2);
    assert_eq!(n.load(Ordering::SeqCst), 2);
}
