use super::*;
use crate::test_http::MockServer;
use reqwest::StatusCode;

#[test]
fn data_comes_back_when_there_are_no_errors() {
    let d = read_graphql(StatusCode::OK, r#"{"data":{"viewer":{"id":"u1"}}}"#).unwrap();
    assert_eq!(d["viewer"]["id"], "u1");
}

#[test]
fn errors_prefer_the_user_presentable_message() {
    let body = r#"{"errors":[{"message":"Entity not found","extensions":{"code":"INVALID_INPUT","userPresentableMessage":"Could not find referenced Issue."}}]}"#;
    let err = read_graphql(StatusCode::BAD_REQUEST, body)
        .unwrap_err()
        .to_string();
    assert_eq!(err, "Linear: Could not find referenced Issue.");
}

#[test]
fn an_authentication_error_says_how_to_fix_it() {
    let body = r#"{"errors":[{"message":"Authentication required, not authenticated","extensions":{"code":"AUTHENTICATION_ERROR"}}]}"#;
    let err = read_graphql(StatusCode::BAD_REQUEST, body)
        .unwrap_err()
        .to_string();
    assert!(err.contains("refused the credential"), "{err}");
    assert!(err.contains("linear login"), "{err}");
}

#[test]
fn a_rate_limit_is_named() {
    let body =
        r#"{"errors":[{"message":"Rate limit exceeded","extensions":{"code":"RATELIMITED"}}]}"#;
    let err = read_graphql(StatusCode::BAD_REQUEST, body)
        .unwrap_err()
        .to_string();
    assert!(err.contains("rate limited"), "{err}");
}

#[test]
fn a_non_json_401_is_still_an_auth_error() {
    let err = read_graphql(StatusCode::UNAUTHORIZED, "nope")
        .unwrap_err()
        .to_string();
    assert!(err.contains("refused the credential"), "{err}");
}

#[tokio::test]
async fn the_query_and_variables_go_out_with_the_header_as_given() {
    let server = MockServer::sequence(vec![serde_json::json!({"data": {"ok": 1}})]);
    let linear = Linear::with_url(MockServer::client(), &server.base, "lin_api_abc".into());
    let d = linear
        .query(
            "query($id: String!) { issue(id: $id) { id } }",
            json!({"id": "ENG-1"}),
        )
        .await
        .unwrap();
    assert_eq!(d["ok"], 1);
    let req = &server.requests()[0];
    assert_eq!(req.method, "POST");
    // API keys go bare; only OAuth tokens get "Bearer".
    assert_eq!(req.headers["authorization"], "lin_api_abc");
    assert_eq!(req.json()["variables"]["id"], "ENG-1");
}
