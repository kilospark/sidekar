//! Local proxy for intercepting agent → LLM API calls.
//!
//! Supports two modes on the same port:
//! - **Reverse proxy**: Agent sets `ANTHROPIC_BASE_URL=https://127.0.0.1:<port>`
//!   and connects directly via TLS. Works with Claude Code.
//! - **MITM proxy**: Agent sets `HTTPS_PROXY=http://127.0.0.1:<port>` and sends
//!   CONNECT tunnels. Works with agents that respect proxy env vars.
//!
//! Detected automatically by peeking the first byte: 0x16 = TLS → reverse proxy,
//! plaintext = CONNECT → MITM proxy.
//!
//! PTY-wrapped agents receive per-tool env overrides from
//! [`crate::agent_cli::build_proxy_child_env`] (universal MITM + CA trust, optional
//! `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL`, and Codex `config.toml` injection).

mod mitm;
mod reverse;

use anyhow::{Result, bail};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

fn proxy_dir() -> PathBuf {
    std::env::temp_dir().join(format!("sidekar-proxy-{}", std::process::id()))
}

// ---------------------------------------------------------------------------
// Codex config.toml ca-certificate injection
// ---------------------------------------------------------------------------

const CODEX_CA_MARKER: &str = "# sidekar-proxy-injected";

fn codex_config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".codex")
        .join("config.toml")
}

/// Inject `ca-certificate` into Codex config.toml. Returns true if modified.
pub fn inject_codex_ca(ca_path: &std::path::Path) -> bool {
    let config_path = codex_config_path();
    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    // Don't double-inject
    if content.contains(CODEX_CA_MARKER) {
        return false;
    }

    let ca_str = ca_path.to_string_lossy();
    let injection = format!("ca-certificate = \"{}\" {}\n", ca_str, CODEX_CA_MARKER);

    // Prepend before any section headers
    let new_content = format!("{injection}{content}");
    std::fs::write(&config_path, new_content).is_ok()
}

/// Remove injected `ca-certificate` from Codex config.toml.
pub fn remove_codex_ca() {
    let config_path = codex_config_path();
    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    if !content.contains(CODEX_CA_MARKER) {
        return;
    }

    let cleaned: String = content
        .lines()
        .filter(|line| !line.contains(CODEX_CA_MARKER))
        .collect::<Vec<_>>()
        .join("\n");

    // Preserve trailing newline
    let _ = std::fs::write(
        &config_path,
        if cleaned.ends_with('\n') {
            cleaned
        } else {
            cleaned + "\n"
        },
    );
}

/// Squash runs of 2+ newlines into a single newline in the raw body.
/// Handles both raw `\n` bytes (pretty-printed JSON) and escaped `\\n`
/// sequences inside JSON string values.
fn squash_newlines(body: &[u8]) -> (Vec<u8>, usize) {
    // Replace runs of escaped newlines: \\n\\n+ → \\n
    // Also squash raw 0x0a runs for pretty-printed JSON.
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        // Check for escaped newline sequence: \n (two bytes: 0x5c 0x6e)
        if i + 1 < body.len() && body[i] == b'\\' && body[i + 1] == b'n' {
            out.push(b'\\');
            out.push(b'n');
            i += 2;
            // Skip consecutive \n sequences
            while i + 1 < body.len() && body[i] == b'\\' && body[i + 1] == b'n' {
                i += 2;
            }
        } else if body[i] == b'\n' {
            out.push(b'\n');
            i += 1;
            // Skip consecutive raw newlines
            while i < body.len() && body[i] == b'\n' {
                i += 1;
            }
        } else {
            out.push(body[i]);
            i += 1;
        }
    }
    let saved = body.len() - out.len();
    (out, saved)
}

/// Resolve upstream from request headers.
/// `x-api-key` or `anthropic-version` → api.anthropic.com
/// `authorization: Bearer ...` → api.openai.com
/// Fallback: api.anthropic.com
fn resolve_upstream(headers: &[(String, String)]) -> (&'static str, u16) {
    for (k, _) in headers {
        let lower = k.to_lowercase();
        if lower == "x-api-key" || lower == "anthropic-version" {
            return ("api.anthropic.com", 443);
        }
    }
    for (k, v) in headers {
        if k.to_lowercase() == "authorization" && v.to_lowercase().starts_with("bearer ") {
            return ("api.openai.com", 443);
        }
    }
    ("api.anthropic.com", 443)
}

struct CachedCert {
    der: rustls::pki_types::CertificateDer<'static>,
    key_der: Vec<u8>,
}

struct ProxyState {
    ca_cert: rcgen::Certificate,
    ca_key: rcgen::KeyPair,
    tls_connector: tokio_rustls::TlsConnector,
    host_cache: RwLock<HashMap<String, Arc<CachedCert>>>,
    verbose: bool,
    log_tx: tokio::sync::mpsc::UnboundedSender<ProxyLogEntry>,
}

struct ProxyLogEntry {
    method: String,
    path: String,
    upstream_host: String,
    request_headers: Vec<(String, String)>,
    request_body: Vec<u8>,
    response_status: u16,
    response_headers: Vec<(String, String)>,
    response_body: Vec<u8>,
    duration_ms: u64,
}

const MAX_RESPONSE_CAPTURE: usize = 10 * 1024 * 1024; // 10MB

async fn proxy_log_writer(mut rx: tokio::sync::mpsc::UnboundedReceiver<ProxyLogEntry>) {
    while let Some(entry) = rx.recv().await {
        let _ = tokio::task::spawn_blocking(move || {
            let req_hdrs = serde_json::to_string(&entry.request_headers).unwrap_or_default();
            let resp_hdrs = serde_json::to_string(&entry.response_headers).unwrap_or_default();
            let _ = crate::broker::proxy_log_insert(&crate::broker::ProxyLogEntry {
                method: entry.method,
                path: entry.path,
                upstream_host: entry.upstream_host,
                request_headers: req_hdrs,
                request_body: entry.request_body,
                response_status: entry.response_status,
                response_headers: resp_hdrs,
                response_body: entry.response_body,
                duration_ms: entry.duration_ms,
            });
        })
        .await;
    }
}

/// Ephemeral ports are reused quickly; parallel tests (or back-to-back `start()`)
/// can get the same port. CA path must be unique per instance so one `cleanup_ca_file`
/// does not delete another proxy's PEM.
static CA_PEM_SEQ: AtomicU64 = AtomicU64::new(0);

/// Start the proxy. Returns `(port, ca_cert_path)`.
pub async fn start(verbose: bool) -> Result<(u16, PathBuf)> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Generate ephemeral CA
    let ca_key = rcgen::KeyPair::generate()?;
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new())?;
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Sidekar Local CA");
    ca_params
        .key_usages
        .push(rcgen::KeyUsagePurpose::DigitalSignature);
    ca_params
        .key_usages
        .push(rcgen::KeyUsagePurpose::KeyCertSign);
    let ca_cert = ca_params.self_signed(&ca_key)?;

    // TLS connector for outbound (proxy → real API server)
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let tls_connector = tokio_rustls::TlsConnector::from(Arc::new(client_config));

    let dir = proxy_dir();
    std::fs::create_dir_all(&dir)?;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();

    let seq = CA_PEM_SEQ.fetch_add(1, Ordering::Relaxed);
    let ca_pem_path = dir.join(format!("ca-{port}-{seq}.pem"));
    std::fs::write(&ca_pem_path, ca_cert.pem())?;

    let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(proxy_log_writer(log_rx));

    // Prune entries older than 7 days on startup
    let _ = tokio::task::spawn_blocking(|| crate::broker::proxy_log_prune(7 * 86400)).await;

    let state = Arc::new(ProxyState {
        ca_cert,
        ca_key,
        tls_connector,
        host_cache: RwLock::new(HashMap::new()),
        verbose,
        log_tx,
    });

    if verbose {
        crate::broker::try_log_event(
            "debug",
            "proxy",
            &format!("listening on 127.0.0.1:{port} (reverse + MITM)"),
            None,
        );
    }

    tokio::spawn(accept_loop(listener, state));

    Ok((port, ca_pem_path))
}

/// Clean up the CA PEM file written by `start()`.
pub fn cleanup_ca_file(ca_path: &std::path::Path) {
    let _ = std::fs::remove_file(ca_path);
}

async fn accept_loop(listener: TcpListener, state: Arc<ProxyState>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let st = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(st, stream).await {
                        crate::broker::try_log_error("proxy", &format!("{e:#}"), None);
                    }
                });
            }
            Err(_) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Connection dispatch — peek first byte to determine mode
// ---------------------------------------------------------------------------

async fn handle_connection(state: Arc<ProxyState>, stream: TcpStream) -> Result<()> {
    let mut peek = [0u8; 8];
    let n = stream.peek(&mut peek).await?;

    if n > 0 && peek[0] == 0x16 {
        // TLS ClientHello → MITM CONNECT proxy (agents using HTTPS_PROXY)
        // Note: not used for reverse proxy — we use plain HTTP for that
        mitm::handle_connect_proxy(state, stream).await
    } else {
        // Plaintext HTTP — either CONNECT tunnel or reverse proxy
        let prefix = std::str::from_utf8(&peek[..n]).unwrap_or("");
        if prefix.starts_with("CONNECT") || "CONNECT".starts_with(prefix) {
            mitm::handle_connect_proxy(state, stream).await
        } else {
            // Direct HTTP request → reverse proxy (ANTHROPIC_BASE_URL=http://...)
            reverse::handle_reverse_proxy_http(state, stream).await
        }
    }
}

#[cfg(test)]
mod tests {
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
}
