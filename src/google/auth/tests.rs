use super::*;

fn tags(id: &str, secret: &str, acct: &str) -> Vec<String> {
    tags_for(id, secret, acct)
}

#[test]
fn a_token_records_the_client_that_minted_it() {
    // Nothing connects an account to a client: a Workspace client is Internal
    // and refuses outside addresses, a personal account needs its own External
    // client, and one client can serve many accounts. So it is recorded, not
    // derived.
    let t = token_ref_from(
        "GOOGLE_KS_TOKEN",
        &tags("GOOGLE_KS_ID", "GOOGLE_KS_SECRET", "hello@kilospark.com"),
    )
    .expect("should parse");
    assert_eq!(t.key, "GOOGLE_KS_TOKEN");
    assert_eq!(t.client_id_key, "GOOGLE_KS_ID");
    assert_eq!(t.client_secret_key, "GOOGLE_KS_SECRET");
    assert_eq!(t.account, "hello@kilospark.com");
}

#[test]
fn any_key_name_works_because_none_is_imposed() {
    // The caller's existing convention must survive untouched.
    for key in ["GOOGLE_OAUTH_CLIENT_ID", "my.weird-key_1", "NB"] {
        let t = token_ref_from(key, &tags("ID", "SECRET", "a@b.com")).expect("should parse");
        assert_eq!(t.key, key);
    }
}

#[test]
fn an_address_with_a_dot_is_kept_verbatim() {
    // The earlier design slugged the address into the key, where karthik.nb@
    // and karthik_nb@ collided. Storing it avoids the question entirely.
    let t = token_ref_from("T", &tags("ID", "SECRET", "karthik.nb@gmail.com")).unwrap();
    assert_eq!(t.account, "karthik.nb@gmail.com");
}

#[test]
fn an_entry_without_client_tags_is_not_a_google_token() {
    // Other things live in kv; only entries carrying both client keys are usable.
    assert!(token_ref_from("SOMETHING", &["google".into()]).is_none());
    assert!(token_ref_from("T", &[format!("{CLIENT_ID_TAG}ID")]).is_none());
    assert!(token_ref_from("T", &[]).is_none());
}

#[test]
fn a_token_with_no_recorded_account_still_works() {
    // userinfo can fail without the grant being useless.
    let t = token_ref_from(
        "T",
        &[
            format!("{CLIENT_ID_TAG}ID"),
            format!("{CLIENT_SECRET_TAG}SECRET"),
        ],
    )
    .expect("client keys are what make it usable");
    assert_eq!(t.account, "");
}

#[test]
fn query_params_decodes_and_splits() {
    let p = query_params("/?code=4%2F0AX&state=abc123");
    assert_eq!(p.get("code").map(String::as_str), Some("4/0AX"));
    assert_eq!(p.get("state").map(String::as_str), Some("abc123"));
}

#[test]
fn query_params_keeps_an_error_redirect_readable() {
    let p = query_params("/?error=access_denied&state=xyz");
    assert_eq!(p.get("error").map(String::as_str), Some("access_denied"));
    assert!(query_params("/").is_empty());
}

#[test]
fn scopes_cover_all_five_apis_without_asking_for_deletion() {
    let joined = SCOPES.join(" ");
    for needed in [
        "gmail.modify",
        "auth/drive",
        "auth/calendar",
        "spreadsheets",
        "documents",
    ] {
        assert!(joined.contains(needed), "missing {needed}");
    }
    // mail.google.com would add permanent deletion, the one Gmail action with
    // no undo. gmail.modify deliberately stops short of it.
    assert!(!joined.contains("https://mail.google.com"));
}
