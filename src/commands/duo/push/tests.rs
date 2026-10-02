use super::*;
use serde_json::json;

#[test]
fn a_device_keypair_is_pkcs8_and_spki_pem() {
    let (priv_pem, pub_pem) = generate_device_keypair().unwrap();
    assert!(priv_pem.contains("BEGIN PRIVATE KEY"));
    assert!(pub_pem.contains("BEGIN PUBLIC KEY"));
    // The private key round-trips, which `sign_request` relies on.
    assert!(RsaPrivateKey::from_pkcs8_pem(&priv_pem).is_ok());
}

#[test]
fn signing_is_deterministic_and_carries_the_pkey() {
    let (priv_pem, _) = generate_device_keypair().unwrap();
    let args = (
        priv_pem.as_str(),
        "DPFZ1234",
        "Tue, 01 Oct 2026 20:00:00 -0000",
        "GET",
        "api-abc.duosecurity.com",
        "/push/v2/device/transactions",
        &[][..],
    );
    let a = sign_request(args.0, args.1, args.2, args.3, args.4, args.5, args.6).unwrap();
    let b = sign_request(args.0, args.1, args.2, args.3, args.4, args.5, args.6).unwrap();
    // RSA PKCS#1 v1.5 is deterministic, so the same inputs sign identically.
    assert_eq!(a, b);
    assert!(a.starts_with("Basic "));
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(a.trim_start_matches("Basic "))
        .unwrap();
    let decoded = String::from_utf8(decoded).unwrap();
    assert!(decoded.starts_with("DPFZ1234:"), "pkey prefixes the signature");
}

#[test]
fn post_params_change_the_signature() {
    let (priv_pem, _) = generate_device_keypair().unwrap();
    let base = |params: &[(String, String)]| {
        sign_request(
            &priv_pem,
            "PK",
            "Tue, 01 Oct 2026 20:00:00 -0000",
            "POST",
            "api-abc.duosecurity.com",
            "/push/v2/device/transactions/tx1",
            params,
        )
        .unwrap()
    };
    let approve = base(&[("answer".into(), "approve".into())]);
    let deny = base(&[("answer".into(), "deny".into())]);
    assert_ne!(approve, deny, "the answer is covered by the signature");
}

#[test]
fn the_date_has_the_shape_duo_signs() {
    let d = duo_date();
    // "Wed, 01 Oct 2026 12:34:56 -0000"
    let parts: Vec<&str> = d.split(' ').collect();
    assert_eq!(parts.len(), 6, "{d}");
    assert!(parts[0].ends_with(','));
    assert_eq!(parts[5], "-0000");
    assert_eq!(parts[4].matches(':').count(), 2);
}

#[test]
fn transactions_are_read_from_any_of_duos_id_fields() {
    let body = json!({"response": {"transactions": [
        {"urgid": "u1", "integration_name": "Microsoft 365", "location": "Sacramento, US"},
        {"txid": "t2"},
        {"id": "i3"},
        {"no_id": true},
    ]}});
    let txns = parse_transactions(&body);
    let ids: Vec<&str> = txns.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["u1", "t2", "i3"], "the id-less one is dropped");
    assert_eq!(txns[0].summary, "Microsoft 365 from Sacramento, US");
}

#[test]
fn no_transactions_parses_to_empty() {
    assert!(parse_transactions(&json!({"response": {"transactions": []}})).is_empty());
    assert!(parse_transactions(&json!({"stat": "OK"})).is_empty());
}

#[test]
fn the_rule_approves_one_waits_on_none_and_refuses_many() {
    let t = |id: &str| Transaction { id: id.into(), summary: format!("{id} login") };
    assert_eq!(decide(vec![]), Decision::Wait);
    assert_eq!(decide(vec![t("a")]), Decision::Approve(t("a")));
    match decide(vec![t("a"), t("b")]) {
        Decision::Refuse(s) => assert_eq!(s.len(), 2),
        other => panic!("two pending must refuse, got {other:?}"),
    }
}
