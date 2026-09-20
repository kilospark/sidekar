use super::*;

#[test]
fn query_params_decodes_and_splits() {
    let p = query_params("/?code=4%2F0AX&state=abc123&scope=a+b");
    assert_eq!(p.get("code").map(String::as_str), Some("4/0AX"));
    assert_eq!(p.get("state").map(String::as_str), Some("abc123"));
}

#[test]
fn query_params_on_a_bare_path_is_empty() {
    assert!(query_params("/").is_empty());
    assert!(query_params("").is_empty());
}

#[test]
fn query_params_keeps_an_error_redirect_readable() {
    let p = query_params("/?error=access_denied&state=xyz");
    assert_eq!(p.get("error").map(String::as_str), Some("access_denied"));
}

#[test]
fn scopes_cover_the_three_apis_without_asking_for_deletion() {
    let joined = SCOPES.join(" ");
    assert!(
        joined.contains("gmail.modify"),
        "needs read, send and label"
    );
    assert!(joined.contains("auth/drive"));
    assert!(joined.contains("auth/calendar"));
    // mail.google.com would add permanent deletion, the one Gmail action with
    // no undo. gmail.modify deliberately stops short of it.
    assert!(!joined.contains("https://mail.google.com"));
}
