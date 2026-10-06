use super::*;

fn with_test_home<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    // HOME first: clearing the user id persists, and doing it outside the
    // scratch HOME cleared the developer's real one. The guard restores
    // HOME and removes the directory even if `f` panics.
    let _home = crate::ScratchHome::new();
    crate::broker::clear_current_user_id();
    crate::broker::clear_encryption_key();
    let result = f();
    crate::broker::clear_current_user_id();
    crate::broker::clear_encryption_key();
    result
}

#[test]
fn listing_marks_a_value_that_cant_be_decrypted_instead_of_dropping_it() -> Result<()> {
    with_test_home(|| {
        crate::broker::kv_set("broken", "v", None)?;
        // Under another key, as after a half-finished migration.
        crate::broker::set_encryption_key(vec![0x42u8; 32]);
        crate::broker::kv_set("fine", "v", None)?;

        let items = list_local_kv(None)?;
        let marked: Vec<_> = items
            .iter()
            .map(|e| (e.key.as_str(), e.unreadable.is_some()))
            .collect();
        assert_eq!(marked, [("broken", true), ("fine", false)]);
        Ok(())
    })
}

#[test]
fn normalizes_authenticator_formatted_secrets() {
    assert_eq!(
        normalize_totp_secret("5qgfdbjwysyrw2qb").unwrap(),
        "5QGFDBJWYSYRW2QB"
    );
    assert_eq!(
        normalize_totp_secret(" 5qgf dbjw ysyr w2qb ").unwrap(),
        "5QGFDBJWYSYRW2QB"
    );
    assert_eq!(
        normalize_totp_secret("5qgf-dbjw-ysyr-w2qb").unwrap(),
        "5QGFDBJWYSYRW2QB"
    );
    assert_eq!(
        normalize_totp_secret("KRSXG5CTMVRXEZLU======").unwrap(),
        "KRSXG5CTMVRXEZLU"
    );
}

#[test]
fn rejects_non_base32_secrets() {
    assert!(normalize_totp_secret("  ").is_err());
    assert!(normalize_totp_secret("abcd0189").is_err());
}

#[test]
fn lowercase_secret_decodes_to_ten_bytes() {
    let bytes = totp_secret_bytes("5qgfdbjwysyrw2qb").unwrap();
    assert_eq!(bytes.len(), 10);
    assert_eq!(bytes, totp_secret_bytes("5QGFDBJWYSYRW2QB").unwrap());
}

#[test]
fn rejects_secret_below_eighty_bits() {
    let err = totp_secret_bytes("KRSXG5CT").unwrap_err().to_string();
    assert!(err.contains("80 bits"), "unexpected error: {err}");
}

#[test]
fn secret_name_ref_parses_local_and_remote() {
    let local = SecretNameRef::parse("codex");
    assert_eq!(local.owner, SecretOwner::Local);
    assert_eq!(local.name, "codex");
    assert_eq!(local.display(), "codex");

    let remote = SecretNameRef::parse("kb/codex");
    assert_eq!(
        remote.owner,
        SecretOwner::Remote {
            label: "kb".to_string()
        }
    );
    assert_eq!(remote.name, "codex");
    assert_eq!(remote.display(), "kb/codex");
}

#[test]
fn remote_ref_rejected_by_local_only_operation() {
    let remote = SecretNameRef::parse("kb/key");
    let err = remote
        .ensure_local("kv get")
        .expect_err("remote secret must fail local-only gate");
    assert_eq!(
        err.to_string(),
        "kv get only supports local secrets for now: kb/key"
    );
}

#[test]
fn list_credentials_reads_local_oauth_metadata() -> Result<()> {
    with_test_home(|| {
        let creds = crate::providers::oauth::OAuthCredentials {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: crate::message::epoch_secs() + 3600,
            metadata: json!({
                "provider_type": "codex",
                "email": "dev@example.com"
            }),
        };
        crate::broker::kv_set("oauth:work", &serde_json::to_string(&creds)?, None)?;

        let listed = list_credentials();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "work");
        assert_eq!(listed[0].reference, "work");
        assert_eq!(listed[0].provider, "codex (OpenAI OAuth)");
        assert_eq!(listed[0].email.as_deref(), Some("dev@example.com"));
        assert!(listed[0].local);
        assert_eq!(listed[0].owner, "local");
        Ok(())
    })
}
