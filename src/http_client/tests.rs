use super::*;

fn ca_pem() -> String {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.self_signed(&key).unwrap().pem()
}

fn temp_file(contents: &[u8]) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "sidekar-certs-{}-{}.pem",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, contents).unwrap();
    path
}

#[test]
fn a_bundle_loads_every_certificate_in_both_forms() {
    let bundle = format!("{}{}", ca_pem(), ca_pem());
    let path = temp_file(bundle.as_bytes());
    let roots = load_extra_roots(&path).unwrap();
    assert_eq!(roots.reqwest.len(), 2);
    assert_eq!(roots.der.len(), 2);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_missing_file_says_which() {
    let path = std::env::temp_dir().join("sidekar-no-such-bundle.pem");
    let err = format!("{:#}", load_extra_roots(&path).err().unwrap());
    assert!(err.contains("sidekar-no-such-bundle.pem"), "{err}");
}

#[test]
fn a_file_without_certificates_is_an_error_not_an_empty_trust_list() {
    let path = temp_file(b"this is not a certificate\n");
    let err = format!("{:#}", load_extra_roots(&path).err().unwrap());
    assert!(
        err.contains("no certificates") || err.contains("not a PEM"),
        "{err}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn der_parsing_skips_everything_but_certificates() {
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n{}",
        ca_pem()
    );
    assert_eq!(pem_certs_der(pem.as_bytes()).len(), 1);
}

#[test]
fn the_root_store_still_has_the_built_in_roots() {
    assert!(web_root_store().len() >= webpki_roots::TLS_SERVER_ROOTS.len());
}

/// A TLS WebSocket server on localhost whose certificate a private CA
/// signed, the way an intercepting proxy's is. Returns its port and the CA.
async fn intercepted_ws_server() -> (u16, String) {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let leaf_key = rcgen::KeyPair::generate().unwrap();
    let leaf = rcgen::CertificateParams::new(vec!["localhost".to_string()])
        .unwrap()
        .signed_by(&leaf_key, &ca, &ca_key)
        .unwrap();
    let config = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![leaf.der().clone()],
        rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
    )
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(tls) = acceptor.accept(tcp).await {
                    let _ = tokio_tungstenite::accept_async(tls).await;
                }
            });
        }
    });
    (port, ca.pem())
}

async fn connect(port: u16, roots: rustls::RootCertStore) -> Result<(), String> {
    tokio_tungstenite::connect_async_tls_with_config(
        format!("wss://localhost:{port}/tunnel"),
        None,
        false,
        Some(ws_connector_trusting(roots)),
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[tokio::test]
async fn a_websocket_through_an_intercepting_ca_needs_that_ca_and_works_with_it() {
    let (port, ca) = intercepted_ws_server().await;
    let err = connect(port, web_root_store()).await.unwrap_err();
    assert!(err.contains("UnknownIssuer"), "{err}");
    let mut roots = web_root_store();
    for cert in pem_certs_der(ca.as_bytes()) {
        roots.add(cert).unwrap();
    }
    connect(port, roots).await.unwrap();
}

/// Every connection to a remote host has to go through this module, or it
/// quietly stops trusting `SSL_CERT_FILE`: the first version of this fix
/// covered the Google clients and missed twenty others. Nothing in CI runs
/// clippy, so this is the check that holds the line — `cargo test` runs
/// before every release.
#[test]
fn remote_connections_are_only_built_in_http_client() {
    const FORBIDDEN: &[&str] = &[
        "Client::builder()",
        "Client::new()",
        "connect_async(",
        "RootCertStore::empty()",
    ];
    // Clients that only talk to Chrome's debugging port on localhost, where
    // there is no proxy in the way.
    const LOCALHOST_ONLY: &[&str] = &["src/app_context.rs", "src/pty/chrome.rs"];

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut stack = vec![root.join("src")];
    let mut offenders = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let exempt = !rel.ends_with(".rs")
                || rel == "src/http_client.rs"
                || rel.starts_with("src/http_client/")
                || rel.ends_with("/tests.rs")
                || rel.contains("/tests/")
                || LOCALHOST_ONLY.contains(&rel.as_str());
            if exempt {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for (n, line) in text.lines().enumerate() {
                if let Some(pat) = FORBIDDEN.iter().find(|p| line.contains(*p)) {
                    offenders.push(format!("{rel}:{}: {pat}", n + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "build remote HTTP/TLS/WebSocket clients with crate::http_client \
         (client_builder, blocking_client_builder, web_root_store, ws_connector) \
         so they trust SSL_CERT_FILE:\n  {}",
        offenders.join("\n  ")
    );
}

// ---- WebSockets through an HTTP proxy -----------------------------------------

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: std::collections::HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |k| map.get(k).cloned()
}

#[test]
fn https_proxy_names_the_proxy_to_connect_through() {
    let proxy = connect_proxy_for(
        "relay.sidekar.dev",
        env_of(&[("HTTPS_PROXY", "http://proxy.corp:3128")]),
    )
    .unwrap()
    .expect("a proxy");
    assert_eq!(proxy.addr, "proxy.corp:3128");
    assert_eq!(proxy.auth, None);

    // Lowercase, no scheme, and ALL_PROXY when nothing more specific is set.
    for env in [
        env_of(&[("https_proxy", "proxy.corp:3128")]),
        env_of(&[("ALL_PROXY", "http://proxy.corp:3128")]),
    ] {
        let proxy = connect_proxy_for("relay.sidekar.dev", env)
            .unwrap()
            .unwrap();
        assert_eq!(proxy.addr, "proxy.corp:3128");
    }
    assert_eq!(
        connect_proxy_for("relay.sidekar.dev", env_of(&[])).unwrap(),
        None
    );
}

#[test]
fn a_proxy_url_with_credentials_authenticates() {
    let proxy = connect_proxy_for(
        "relay.sidekar.dev",
        env_of(&[("HTTPS_PROXY", "http://me:p%40ss@proxy.corp:8080")]),
    )
    .unwrap()
    .unwrap();
    use base64::Engine;
    let expected = base64::engine::general_purpose::STANDARD.encode("me:p@ss");
    assert_eq!(proxy.auth, Some(format!("Basic {expected}")));
}

#[test]
fn no_proxy_exempts_a_host_and_its_subdomains() {
    let proxied = |host: &str, no_proxy: &str| {
        connect_proxy_for(
            host,
            env_of(&[("HTTPS_PROXY", "http://proxy:3128"), ("NO_PROXY", no_proxy)]),
        )
        .unwrap()
        .is_some()
    };
    assert!(!proxied("relay.sidekar.dev", "localhost,sidekar.dev"));
    assert!(!proxied("relay.sidekar.dev", ".sidekar.dev"));
    assert!(!proxied("relay.sidekar.dev", "*"));
    assert!(proxied("relay.sidekar.dev", "notsidekar.dev"));
    assert!(proxied("relay.sidekar.dev", "localhost"));
}

#[test]
fn a_proxy_sidekar_cannot_speak_to_is_an_error_not_a_direct_dial() {
    let err = connect_proxy_for(
        "relay.sidekar.dev",
        env_of(&[("HTTPS_PROXY", "socks5://proxy:1080")]),
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("socks5"), "{err:#}");
}

/// A proxy on a local port that answers one CONNECT with `status`, and
/// reports the request it received.
async fn fake_proxy(status: &'static str) -> (String, tokio::task::JoinHandle<String>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let handle = tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut b = [0u8; 1];
            if conn.read(&mut b).await.unwrap() == 0 {
                break;
            }
            head.push(b[0]);
        }
        conn.write_all(format!("HTTP/1.1 {status}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        // Echo what follows, standing in for the far end of the tunnel.
        let mut buf = [0u8; 64];
        if let Ok(n) = conn.read(&mut buf).await {
            let _ = conn.write_all(&buf[..n]).await;
        }
        String::from_utf8_lossy(&head).to_string()
    });
    (addr, handle)
}

#[tokio::test]
async fn connect_asks_the_proxy_for_the_host_and_hands_back_the_tunnel() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (addr, proxy) = fake_proxy("200 Connection established").await;
    let tcp = tokio::net::TcpStream::connect(&addr).await.unwrap();
    let mut tunnel = connect_through(tcp, "relay.sidekar.dev", 443, Some("Basic eA=="))
        .await
        .unwrap();
    tunnel.write_all(b"hello").await.unwrap();
    let mut echoed = [0u8; 5];
    tunnel.read_exact(&mut echoed).await.unwrap();
    assert_eq!(
        &echoed, b"hello",
        "the stream is the tunnel, nothing consumed"
    );

    let request = proxy.await.unwrap();
    assert!(
        request.starts_with("CONNECT relay.sidekar.dev:443 HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(
        request.contains("Proxy-Authorization: Basic eA==\r\n"),
        "{request}"
    );
}

#[tokio::test]
async fn a_refused_connect_says_what_the_proxy_answered() {
    let (addr, _proxy) = fake_proxy("407 Proxy Authentication Required").await;
    let tcp = tokio::net::TcpStream::connect(&addr).await.unwrap();
    let err = connect_through(tcp, "relay.sidekar.dev", 443, None)
        .await
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("407 Proxy Authentication Required"),
        "{err:#}"
    );
}

#[test]
fn the_usual_tls_failures_are_explained() {
    let plain = anyhow::anyhow!("IO error: received corrupt message of type InvalidContentType");
    let why = explain_tls_failure(&plain, "relay.sidekar.dev").unwrap();
    assert!(why.contains("HTTPS_PROXY"), "{why}");

    let issuer = anyhow::anyhow!("invalid peer certificate: UnknownIssuer");
    let why = explain_tls_failure(&issuer, "relay.sidekar.dev").unwrap();
    assert!(why.contains("SSL_CERT_FILE"), "{why}");

    let other = anyhow::anyhow!("connection refused");
    assert_eq!(explain_tls_failure(&other, "relay.sidekar.dev"), None);
}
