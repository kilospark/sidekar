use super::*;

#[test]
fn proxy_dir_does_not_depend_on_home() {
    let _guard = crate::test_home_lock()
        .lock()
        .unwrap_or_else(|_| panic!("failed to lock test HOME mutex"));
    let old_home = std::env::var_os("HOME");
    let fake_home =
        std::env::temp_dir().join(format!("sidekar-proxy-home-test-{}", std::process::id()));
    std::fs::create_dir_all(&fake_home).expect("create fake home");
    unsafe { std::env::set_var("HOME", &fake_home) };

    let dir = proxy_dir();

    match old_home {
        Some(home) => unsafe { std::env::set_var("HOME", home) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    let _ = std::fs::remove_dir_all(&fake_home);

    assert!(dir.starts_with(std::env::temp_dir()));
    assert!(!dir.starts_with(&fake_home));
}

/// Build a client that uses CONNECT proxy (MITM mode).
async fn mitm_client(port: u16, ca_path: &std::path::Path) -> reqwest::Client {
    let ca_pem =
        std::fs::read(ca_path).unwrap_or_else(|e| panic!("read {}: {e}", ca_path.display()));
    let ca_cert = reqwest::Certificate::from_pem(&ca_pem).expect("parse ca cert");
    reqwest::Client::builder()
        .proxy(reqwest::Proxy::https(format!("http://127.0.0.1:{port}")).unwrap())
        .add_root_certificate(ca_cert)
        .build()
        .expect("build client")
}

/// Build a plain HTTP client (reverse proxy mode — no TLS on local leg).
fn reverse_client() -> reqwest::Client {
    reqwest::Client::builder().build().expect("build client")
}

#[tokio::test]
async fn mitm_passthrough() {
    let (port, ca_path) = start(true).await.expect("proxy start");
    let client = mitm_client(port, &ca_path).await;

    let resp = client
        .get("https://httpbin.org/get")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .expect("request through MITM proxy");

    assert!(resp.status().is_success());
    cleanup_ca_file(&ca_path);
}

#[tokio::test]
async fn mitm_anthropic() {
    let (port, ca_path) = start(true).await.expect("proxy start");
    let client = mitm_client(port, &ca_path).await;

    let resp = client
        .get("https://api.anthropic.com/v1/models")
        .header("x-api-key", "test-invalid")
        .header("anthropic-version", "2023-06-01")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .expect("request through MITM proxy to anthropic");

    assert_eq!(resp.status().as_u16(), 401);
    cleanup_ca_file(&ca_path);
}

#[tokio::test]
async fn reverse_proxy_anthropic() {
    let (port, ca_path) = start(true).await.expect("proxy start");
    let client = reverse_client();

    // Simulate ANTHROPIC_BASE_URL=http://127.0.0.1:<port>
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .header("x-api-key", "test-invalid")
        .header("anthropic-version", "2023-06-01")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .expect("request through reverse proxy");

    // 401/403 = request reached upstream (403 if CDN rejects Host mismatch)
    let s = resp.status().as_u16();
    assert!(s == 401 || s == 403, "unexpected status: {s}");
    cleanup_ca_file(&ca_path);
}

#[tokio::test]
async fn reverse_proxy_openai() {
    let (port, ca_path) = start(true).await.expect("proxy start");
    let client = reverse_client();

    // Simulate OPENAI_BASE_URL=http://127.0.0.1:<port>/v1
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .header("authorization", "Bearer test-invalid")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .expect("request through reverse proxy to openai");

    let s = resp.status().as_u16();
    assert!(s == 401 || s == 403, "unexpected status: {s}");
    cleanup_ca_file(&ca_path);
}
