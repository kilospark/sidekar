use super::*;
use std::io::Read;

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
fn query_params_reads_a_plus_as_a_space() {
    // Form encoding, which Slack uses for error descriptions.
    let p = query_params("/callback?error=invalid+team&state=s");
    assert_eq!(p.get("error").map(String::as_str), Some("invalid team"));
}

fn redirect(port: u16, path: &str) -> String {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
        .unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

#[test]
fn the_listener_ignores_favicon_requests_and_returns_the_code() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h =
        std::thread::spawn(move || wait_for_code(vec![l], "st8", "Test", Duration::from_secs(10)));
    let fav = redirect(port, "/favicon.ico");
    assert!(fav.starts_with("HTTP/1.1 404"), "{fav}");
    let ok = redirect(port, "/callback?code=abc&state=st8");
    assert!(ok.contains("Signed in"), "{ok}");
    assert_eq!(h.join().unwrap().unwrap(), "abc");
}

#[test]
fn a_wrong_state_is_a_404_and_the_listener_keeps_waiting() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = std::thread::spawn(move || {
        wait_for_code(vec![l], "expected", "Test", Duration::from_secs(10))
    });
    let forged = redirect(port, "/callback?code=evil&state=forged");
    assert!(forged.starts_with("HTTP/1.1 404"), "{forged}");
    let missing = redirect(port, "/callback?code=evil");
    assert!(missing.starts_with("HTTP/1.1 404"), "{missing}");
    redirect(port, "/callback?code=good&state=expected");
    assert_eq!(h.join().unwrap().unwrap(), "good");
}

#[test]
fn a_forged_error_cannot_cancel_the_login() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h =
        std::thread::spawn(move || wait_for_code(vec![l], "real", "Test", Duration::from_secs(10)));
    let forged = redirect(port, "/callback?error=access_denied&state=guess");
    assert!(forged.starts_with("HTTP/1.1 404"), "{forged}");
    let no_state = redirect(port, "/callback?error=access_denied");
    assert!(no_state.starts_with("HTTP/1.1 404"), "{no_state}");
    redirect(port, "/callback?code=c&state=real");
    assert_eq!(h.join().unwrap().unwrap(), "c");
}

#[test]
fn an_idle_connection_does_not_block_the_redirect() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let started = Instant::now();
    let h =
        std::thread::spawn(move || wait_for_code(vec![l], "s1", "Test", Duration::from_secs(30)));
    // Connects first and never sends a byte, like a browser preconnect.
    let _idle = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let ok = redirect(port, "/callback?code=abc&state=s1");
    assert!(ok.contains("Signed in"), "{ok}");
    assert_eq!(h.join().unwrap().unwrap(), "abc");
    assert!(
        started.elapsed() < REQUEST_READ_TIMEOUT + Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn the_overall_timeout_holds_even_with_a_connection_open() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let started = Instant::now();
    let h = std::thread::spawn(move || wait_for_code(vec![l], "s", "Test", Duration::from_secs(1)));
    let _idle = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let err = h.join().unwrap().unwrap_err().to_string();
    assert!(err.contains("timed out"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_request_line_split_across_packets_is_read_whole() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h =
        std::thread::spawn(move || wait_for_code(vec![l], "s", "Test", Duration::from_secs(10)));
    let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.write_all(b"GET /callback?code=x").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    c.write_all(b"y&state=s HTTP/1.1\r\n\r\n").unwrap();
    assert_eq!(h.join().unwrap().unwrap(), "xy");
}

#[test]
fn states_are_128_random_bits() {
    let a = random_state();
    assert_eq!(a.len(), 32);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, random_state());
}

#[test]
fn pkce_matches_the_rfc_7636_example() {
    // RFC 7636 appendix B.
    let p = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
    assert_eq!(p.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    assert_eq!(
        p.query(),
        "&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256"
    );
    let fresh = Pkce::new();
    assert!(fresh.verifier.len() >= 43, "RFC 7636 minimum length");
}

#[test]
fn a_taken_ipv6_port_is_an_error_not_a_silent_skip() {
    let busy = std::io::Error::from(std::io::ErrorKind::AddrInUse);
    assert!(!ipv6_absent(&busy));
    assert!(ipv6_absent(&std::io::Error::from(
        std::io::ErrorKind::AddrNotAvailable
    )));
    // Hold [::1]:<port> elsewhere, then ask for it: refuse rather than let
    // that process receive the code.
    if let Ok(squatter) = TcpListener::bind(("::1", 0)) {
        let port = squatter.local_addr().unwrap().port();
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            // (the v4 probe above is dropped again before bind_localhost)
            let err = bind_localhost(port).unwrap_err();
            assert!(format!("{err:#}").contains("[::1]"), "{err:#}");
        }
    }
}

#[test]
fn the_listener_names_the_provider_that_refused() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h =
        std::thread::spawn(move || wait_for_code(vec![l], "s", "Slack", Duration::from_secs(10)));
    redirect(port, "/callback?error=access_denied&state=s");
    let err = h.join().unwrap().unwrap_err().to_string();
    assert!(err.contains("Slack refused"), "{err}");
}

#[test]
fn a_bare_token_round_trips_unchanged() {
    // `slack add` / `linear add` re-store whatever the user put in kv; it must
    // come back exactly as it went in.
    let t = ExpiringToken::parse("xoxp-123-abc");
    assert_eq!(t.access_token, "xoxp-123-abc");
    assert!(!t.needs_refresh(u64::MAX / 2));
    assert_eq!(t.to_value(), "xoxp-123-abc");
}

#[test]
fn an_expiring_token_round_trips_through_its_blob() {
    let resp = json!({"access_token": "a1", "refresh_token": "r1", "expires_in": 86399});
    let t = ExpiringToken::from_response(&resp, 1_000).unwrap();
    assert_eq!(t.expires_at, Some(87_399));
    let back = ExpiringToken::parse(&t.to_value());
    assert_eq!(back, t);
}

#[test]
fn refresh_happens_before_expiry_not_after() {
    let t = ExpiringToken {
        access_token: "a".into(),
        refresh_token: Some("r".into()),
        expires_at: Some(10_000),
    };
    assert!(!t.needs_refresh(1_000));
    // Inside the skew window: refresh now rather than fail mid-command.
    assert!(t.needs_refresh(10_000 - EXPIRY_SKEW_SECS));
    assert!(t.needs_refresh(20_000));
}

#[test]
fn an_expiry_with_no_refresh_token_is_not_refreshable() {
    let t = ExpiringToken {
        access_token: "a".into(),
        refresh_token: None,
        expires_at: Some(10),
    };
    assert!(!t.needs_refresh(1_000));
}

#[test]
fn a_response_without_an_access_token_is_rejected() {
    assert!(ExpiringToken::from_response(&json!({"error": "invalid_grant"}), 0).is_none());
    assert!(ExpiringToken::from_response(&json!({"access_token": ""}), 0).is_none());
}
