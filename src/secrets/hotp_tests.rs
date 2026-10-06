use super::*;
use crate::commands::duo;

/// RFC 4226 Appendix D: the published HOTP codes for the ASCII secret
/// "12345678901234567890", counters 0..=9. These pin the generator.
#[test]
fn matches_the_rfc4226_test_vectors() {
    // base32 of the RFC's ASCII secret.
    let secret = base32_secret(b"12345678901234567890");
    let expected = [
        "755224", "287082", "359152", "969429", "338314", "254676", "287922", "162583",
        "399871", "520489",
    ];
    for (counter, want) in expected.iter().enumerate() {
        assert_eq!(
            hotp_code(&secret, "SHA1", 6, counter as u64).unwrap(),
            *want,
            "counter {counter}"
        );
    }
}

#[test]
fn hex_decode_round_trips_and_rejects_bad_input() {
    assert_eq!(hex_decode("48656c6c6f").unwrap(), b"Hello");
    assert_eq!(hex_decode("00FF").unwrap(), vec![0x00, 0xff]); // case-insensitive
    assert!(hex_decode("abc").is_err(), "odd length");
    assert!(hex_decode("zz").is_err(), "non-hex char");
    assert!(hex_decode("").is_err(), "empty");
}

/// Regression: Duo's hotp_secret is hex, not base32. Enroll used to
/// base32-validate it and reject any secret containing 0/1/8/9, AFTER the
/// single-use activation code was already spent. Decoding hex to bytes and
/// storing base32 produces correct RFC 4226 codes — proven by feeding the
/// RFC's own secret in as hex.
#[test]
fn a_duo_hex_secret_decodes_to_rfc4226_codes() {
    // hex of the ASCII secret "12345678901234567890".
    let hex = "3132333435363738393031323334353637383930";
    let stored = duo_hotp_secret_to_base32(hex).unwrap();
    assert_eq!(stored, base32_secret(b"12345678901234567890"));
    assert_eq!(hotp_code(&stored, "SHA1", 6, 0).unwrap(), "755224");
    assert_eq!(hotp_code(&stored, "SHA1", 6, 1).unwrap(), "287082");
}

/// A real-shaped Duo secret: 32 hex chars (16 bytes) that includes the
/// very digits base32 forbids. The old enroll path rejected exactly this.
#[test]
fn a_32_char_duo_secret_enrolls_without_a_base32_error() {
    let duo = "0123456789abcdef8899aabbccddeeff";
    assert!(normalize_totp_secret(duo).is_err(), "it is not valid base32");
    let stored = duo_hotp_secret_to_base32(duo).unwrap();
    let code = hotp_code(&stored, "SHA1", 6, 0).unwrap();
    assert_eq!(code.len(), 6);
    assert!(code.chars().all(|c| c.is_ascii_digit()));
}

#[test]
fn a_duo_activation_url_parses_host_and_code() {
    let (host, code) = duo::parse_activation(
        "https://api-abc1234.duosecurity.com/push/v2/activation/DEADBEEFcode1234?foo=bar",
    )
    .unwrap();
    assert_eq!(host, "api-abc1234.duosecurity.com");
    assert_eq!(code, "DEADBEEFcode1234");
    assert!(duo::is_duo_host(&host));
}

#[test]
fn a_non_duo_host_is_refused() {
    assert!(!duo::is_duo_host("evil.example.com"));
    assert!(!duo::is_duo_host("duosecurity.com.evil.com"));
    assert!(duo::is_duo_host("api-x.duosecurity.com"));
}
